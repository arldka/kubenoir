package main

import (
	"encoding/json"

	"k8s.io/apimachinery/pkg/apis/meta/v1/unstructured"
	"k8s.io/apimachinery/pkg/runtime"
	"k8s.io/apimachinery/pkg/util/strategicpatch"
	"k8s.io/kubectl/pkg/util"
)

// Kinds that are not namespaced, of the simulated ones.
var clusterScoped = map[string]bool{"Namespace": true, "Node": true}

// applyCase: `kubectl apply -f manifest.yaml` onto `live.yaml` (the stored object, with the
// last-applied annotation if it has one), as kubectl computes it client side
// (pkg/cmd/apply/patcher.go, builtin-types path): the three-way patch from the last-applied
// configuration, the new configuration (`GetModifiedConfiguration`, annotated) and the live
// object, then the API server's strategic merge of that patch into the live object.
// Writes `expected.yaml` = {patch, result}; an empty patch means `unchanged` (no request).
func applyCase(c *caseDir) error {
	live, err := c.readJSON("live.yaml")
	if err != nil {
		return err
	}
	manifest, err := c.readJSON("manifest.yaml")
	if err != nil {
		return err
	}

	obj := &unstructured.Unstructured{}
	if err := obj.UnmarshalJSON(manifest); err != nil {
		return err
	}
	// The resource builder fills in the namespace of a namespaced object.
	if !clusterScoped[obj.GetKind()] && obj.GetNamespace() == "" {
		obj.SetNamespace("default")
	}
	modified, err := util.GetModifiedConfiguration(obj, true, unstructured.UnstructuredJSONScheme)
	if err != nil {
		return err
	}

	liveObj := &unstructured.Unstructured{}
	if err := liveObj.UnmarshalJSON(live); err != nil {
		return err
	}
	current, err := runtime.Encode(unstructured.UnstructuredJSONScheme, liveObj)
	if err != nil {
		return err
	}
	original, err := util.GetOriginalConfiguration(liveObj)
	if err != nil {
		return err
	}

	typed, err := typedObject(manifest)
	if err != nil {
		return err
	}
	meta, err := strategicpatch.NewPatchMetaFromStruct(typed)
	if err != nil {
		return err
	}
	patch, err := strategicpatch.CreateThreeWayMergePatch(original, modified, current, meta, true)
	if err != nil {
		return c.writeErr(err)
	}

	result := current
	if string(patch) != "{}" {
		result, err = strategicpatch.StrategicMergePatch(current, patch, typed)
		if err != nil {
			return c.writeErr(err)
		}
	}
	out, err := json.Marshal(map[string]json.RawMessage{"patch": patch, "result": result})
	if err != nil {
		return err
	}
	return c.writeYAML("expected.yaml", out)
}
