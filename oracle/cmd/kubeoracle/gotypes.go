package main

import (
	"encoding"
	"encoding/json"
	"fmt"
	"os"
	"reflect"
	"runtime/debug"
	"strings"

	"k8s.io/apimachinery/pkg/api/resource"
	metav1 "k8s.io/apimachinery/pkg/apis/meta/v1"
	"k8s.io/apimachinery/pkg/runtime"
	"k8s.io/apimachinery/pkg/runtime/schema"
	"k8s.io/apimachinery/pkg/util/intstr"
	"k8s.io/client-go/kubernetes/scheme"
)

// The kinds the engine simulates (SPEC C18; `api::kinds()` in the engine).
var simulatedKinds = []schema.GroupVersionKind{
	{Version: "v1", Kind: "ConfigMap"},
	{Group: "batch", Version: "v1", Kind: "CronJob"},
	{Group: "apps", Version: "v1", Kind: "Deployment"},
	{Version: "v1", Kind: "Endpoints"},
	{Group: "discovery.k8s.io", Version: "v1", Kind: "EndpointSlice"},
	{Version: "v1", Kind: "Event"},
	{Group: "batch", Version: "v1", Kind: "Job"},
	{Version: "v1", Kind: "Namespace"},
	{Group: "networking.k8s.io", Version: "v1", Kind: "NetworkPolicy"},
	{Version: "v1", Kind: "Node"},
	{Version: "v1", Kind: "Pod"},
	{Group: "apps", Version: "v1", Kind: "ReplicaSet"},
	{Version: "v1", Kind: "Secret"},
	{Version: "v1", Kind: "Service"},
}

// Types that encode themselves (json.Marshaler) and how the engine writes them.
var marshalers = map[reflect.Type]string{
	reflect.TypeOf(resource.Quantity{}):    "Quantity",
	reflect.TypeOf(metav1.Time{}):          "Time",
	reflect.TypeOf(metav1.MicroTime{}):     "Time",
	reflect.TypeOf(intstr.IntOrString{}):   "IntOrString",
	reflect.TypeOf(runtime.RawExtension{}): "Raw",
	reflect.TypeOf(metav1.FieldsV1{}):      "Raw",
	reflect.TypeOf(json.RawMessage{}):      "Raw",
}

var (
	jsonMarshaler = reflect.TypeOf((*json.Marshaler)(nil)).Elem()
	textMarshaler = reflect.TypeOf((*encoding.TextMarshaler)(nil)).Elem()
)

// A node of the Go type graph reachable from the simulated kinds: what upstream's strategic
// merge patch reads by reflection (`patchStrategy`, `patchMergeKey`), and what encoding/json
// needs to write an object the way the API server stores it (omitempty / omitzero, types
// that marshal themselves).
type typeNode struct {
	goKind    string // reflect.Kind name: "struct", "slice", "map", "ptr", "string", ...
	name      string // struct type name, for error texts
	elem      int    // slice, map, ptr: element node
	marshaler string // a type that encodes itself: "Quantity", "Time", ...
	fields    []typeField
}

type typeField struct {
	json       string
	strategies string // the `patchStrategy` tag as written, e.g. "merge,retainKeys"
	mergeKey   string // the `patchMergeKey` tag
	omitEmpty  bool
	omitZero   bool
	node       int
}

type typeGraph struct {
	nodes []typeNode
	index map[reflect.Type]int
	err   error
}

func (g *typeGraph) add(t reflect.Type) int {
	if i, ok := g.index[t]; ok {
		return i
	}
	i := len(g.nodes)
	g.index[t] = i
	g.nodes = append(g.nodes, typeNode{goKind: t.Kind().String(), name: t.Name()})
	if m, ok := marshalers[t]; ok {
		g.nodes[i].marshaler = m
		return i
	}
	if t.Kind() == reflect.Slice && t.Elem().Kind() == reflect.Uint8 {
		g.nodes[i].marshaler = "Bytes"
		return i
	}
	if t.Kind() != reflect.Pointer && (reflect.PointerTo(t).Implements(jsonMarshaler) || reflect.PointerTo(t).Implements(textMarshaler)) {
		// A type that encodes itself in a way the engine does not know: fail rather than
		// write it wrongly.
		if g.err == nil {
			g.err = fmt.Errorf("%s encodes itself (MarshalJSON/MarshalText); add it to marshalers", t)
		}
	}
	switch t.Kind() {
	case reflect.Slice, reflect.Array, reflect.Map, reflect.Pointer:
		elem := g.add(t.Elem())
		g.nodes[i].elem = elem
	case reflect.Struct:
		fields := g.structFields(t)
		g.nodes[i].fields = fields
	}
	return i
}

// structFields lists a struct's JSON fields the way encoding/json (and upstream's forked copy
// that strategic merge patch uses) sees them: in field order, embedded structs without a JSON
// name flattened in place, `json:"-"` and unexported fields skipped.
func (g *typeGraph) structFields(t reflect.Type) []typeField {
	var out []typeField
	seen := map[string]bool{}
	var walk func(t reflect.Type)
	walk = func(t reflect.Type) {
		for i := 0; i < t.NumField(); i++ {
			f := t.Field(i)
			tag := f.Tag.Get("json")
			if tag == "-" {
				continue
			}
			name, opts, _ := strings.Cut(tag, ",")
			ft := f.Type
			if f.Anonymous && name == "" {
				if ft.Kind() == reflect.Pointer {
					ft = ft.Elem()
				}
				if ft.Kind() == reflect.Struct {
					walk(ft)
					continue
				}
			}
			if !f.IsExported() {
				continue
			}
			if name == "" {
				name = f.Name
			}
			// A shallower field of the same name wins (encoding/json's dominance rule); k8s
			// types never rely on the subtler cases.
			if seen[name] {
				continue
			}
			seen[name] = true
			options := "," + opts + ","
			out = append(out, typeField{
				json:       name,
				strategies: f.Tag.Get("patchStrategy"),
				mergeKey:   f.Tag.Get("patchMergeKey"),
				omitEmpty:  strings.Contains(options, ",omitempty,"),
				omitZero:   strings.Contains(options, ",omitzero,"),
				node:       g.add(f.Type),
			})
		}
	}
	walk(t)
	return out
}

// gotypes writes the engine's Go type graph (engine/src/gotypes.rs).
func gotypes(out string) error {
	g := &typeGraph{index: map[reflect.Type]int{}}
	type root struct {
		apiVersion, kind string
		node             int
	}
	var roots []root
	for _, gvk := range simulatedKinds {
		obj, err := scheme.Scheme.New(gvk)
		if err != nil {
			return err
		}
		apiVersion, kind := gvk.ToAPIVersionAndKind()
		roots = append(roots, root{apiVersion, kind, g.add(reflect.TypeOf(obj).Elem())})
	}
	if g.err != nil {
		return g.err
	}

	var b strings.Builder
	fmt.Fprintf(&b, "// @generated by `kubeoracle gotypes` (make oracle-goldens) from k8s.io/api %s.\n", moduleVersion("k8s.io/api"))
	b.WriteString("// Do not edit. The Go types reachable from the simulated kinds, as upstream sees them by\n")
	b.WriteString("// reflection: strategic merge patch's struct tags (`patchStrategy`, `patchMergeKey`) and\n")
	b.WriteString("// encoding/json's field options (`omitempty`, `omitzero`) and self-encoding types.\n\n")
	b.WriteString("use crate::gojson::{Field, Marshal, Node};\n\n")
	b.WriteString("/// `(apiVersion, kind)` → its node in [`NODES`].\n")
	b.WriteString("#[rustfmt::skip]\n")
	b.WriteString("pub static ROOTS: &[(&str, &str, u16)] = &[\n")
	for _, r := range roots {
		fmt.Fprintf(&b, "    (%q, %q, %d),\n", r.apiVersion, r.kind, r.node)
	}
	b.WriteString("];\n\n")
	b.WriteString("#[rustfmt::skip]\n")
	b.WriteString("pub static NODES: &[Node] = &[\n")
	for i, n := range g.nodes {
		switch {
		case n.marshaler != "":
			fmt.Fprintf(&b, "    /* %d */ Node::Marshal { go_kind: %q, name: %q, marshal: Marshal::%s },\n", i, n.goKind, n.name, n.marshaler)
		case n.goKind == "struct":
			fmt.Fprintf(&b, "    /* %d */ Node::Struct { name: %q, fields: &[\n", i, n.name)
			for _, f := range n.fields {
				fmt.Fprintf(&b, "        Field { json: %q, strategies: %q, merge_key: %q, omit_empty: %t, omit_zero: %t, node: %d },\n",
					f.json, f.strategies, f.mergeKey, f.omitEmpty, f.omitZero, f.node)
			}
			b.WriteString("    ] },\n")
		case n.goKind == "slice" || n.goKind == "array":
			fmt.Fprintf(&b, "    /* %d */ Node::Slice(%d),\n", i, n.elem)
		case n.goKind == "map":
			fmt.Fprintf(&b, "    /* %d */ Node::Map(%d),\n", i, n.elem)
		case n.goKind == "ptr":
			fmt.Fprintf(&b, "    /* %d */ Node::Ptr(%d),\n", i, n.elem)
		default:
			fmt.Fprintf(&b, "    /* %d */ Node::Scalar(%q),\n", i, n.goKind)
		}
	}
	b.WriteString("];\n")
	return os.WriteFile(out, []byte(b.String()), 0o644)
}

func moduleVersion(path string) string {
	info, ok := debug.ReadBuildInfo()
	if ok {
		for _, dep := range info.Deps {
			if dep.Path == path {
				return dep.Version
			}
		}
	}
	return "(unknown version)"
}
