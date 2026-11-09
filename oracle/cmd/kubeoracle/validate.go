package main

import (
	"context"
	"encoding/json"
	"fmt"
	"strings"

	apierrors "k8s.io/apimachinery/pkg/api/errors"
	"k8s.io/apimachinery/pkg/api/meta"
	metav1 "k8s.io/apimachinery/pkg/apis/meta/v1"
	"k8s.io/apimachinery/pkg/runtime"
	"k8s.io/apimachinery/pkg/runtime/schema"
	genericapirequest "k8s.io/apiserver/pkg/endpoints/request"
	"k8s.io/apiserver/pkg/registry/rest"
	"k8s.io/kubernetes/pkg/api/legacyscheme"
	api "k8s.io/kubernetes/pkg/apis/core"
	"k8s.io/kubernetes/pkg/registry/apps/deployment"
	"k8s.io/kubernetes/pkg/registry/apps/replicaset"
	"k8s.io/kubernetes/pkg/registry/batch/cronjob"
	"k8s.io/kubernetes/pkg/registry/batch/job"
	"k8s.io/kubernetes/pkg/registry/core/configmap"
	"k8s.io/kubernetes/pkg/registry/core/endpoint"
	"k8s.io/kubernetes/pkg/registry/core/event"
	"k8s.io/kubernetes/pkg/registry/core/namespace"
	"k8s.io/kubernetes/pkg/registry/core/node"
	"k8s.io/kubernetes/pkg/registry/core/pod"
	"k8s.io/kubernetes/pkg/registry/core/secret"
	"k8s.io/kubernetes/pkg/registry/core/service"
	"k8s.io/kubernetes/pkg/registry/discovery/endpointslice"
	"k8s.io/kubernetes/pkg/registry/networking/networkpolicy"
	"sigs.k8s.io/yaml"
)

type strategy interface {
	rest.RESTCreateStrategy
	rest.RESTUpdateStrategy
}

// The registry strategy the API server uses for each simulated kind.
var strategies = map[string]strategy{
	"ConfigMap":     configmap.Strategy,
	"CronJob":       cronjob.Strategy,
	"Deployment":    deployment.Strategy,
	"Endpoints":     endpoint.Strategy,
	"EndpointSlice": endpointslice.Strategy,
	"Event":         event.Strategy,
	"Job":           job.Strategy,
	"Namespace":     namespace.Strategy,
	"NetworkPolicy": networkpolicy.Strategy,
	"Node":          node.Strategy,
	"Pod":           pod.Strategy,
	"ReplicaSet":    replicaset.Strategy,
	"Secret":        secret.Strategy,
	"Service":       service.Strategy,
}

// internalObject decodes a manifest the way the API server's write path does: into its
// versioned type, defaulted, converted to the internal type the strategies work on.
func internalObject(manifestJSON []byte) (runtime.Object, string, error) {
	var meta struct {
		APIVersion string `json:"apiVersion"`
		Kind       string `json:"kind"`
	}
	if err := yaml.Unmarshal(manifestJSON, &meta); err != nil {
		return nil, "", err
	}
	gv, err := schema.ParseGroupVersion(meta.APIVersion)
	if err != nil {
		return nil, "", err
	}
	versioned, err := legacyscheme.Scheme.New(gv.WithKind(meta.Kind))
	if err != nil {
		return nil, "", err
	}
	if err := json.Unmarshal(manifestJSON, versioned); err != nil {
		return nil, "", fmt.Errorf("decoding: %w", err)
	}
	legacyscheme.Scheme.Default(versioned)
	internal, err := legacyscheme.Scheme.New(gv.WithKind(meta.Kind).GroupKind().WithVersion(runtime.APIVersionInternal))
	if err != nil {
		return nil, "", err
	}
	if err := legacyscheme.Scheme.Convert(versioned, internal, nil); err != nil {
		return nil, "", err
	}
	return internal, meta.Kind, nil
}

// validateCase: `manifest.yaml` created (or, with `old.yaml`, updated from that object) through
// the kind's registry strategy as the API server does it (rest.BeforeCreate / BeforeUpdate:
// prepare, strategy validation, then generic metadata validation). Writes `expected.err` with
// the status causes as kubectl prints them (`<field>: <message>`, one per line), or
// `expected.yaml` = `valid: true`.
func validateCase(c *caseDir) error {
	manifest, err := c.readJSON("manifest.yaml")
	if err != nil {
		return err
	}
	obj, kind, err := internalObject(manifest)
	if err != nil {
		return err
	}
	s, ok := strategies[kind]
	if !ok {
		return fmt.Errorf("no strategy for %s", kind)
	}
	ctx := genericapirequest.NewContext()
	if s.NamespaceScoped() {
		ctx = genericapirequest.WithNamespace(ctx, "default")
	} else {
		ctx = genericapirequest.WithNamespace(ctx, metav1.NamespaceNone)
	}
	ctx = genericapirequest.WithRequestInfo(ctx, &genericapirequest.RequestInfo{APIVersion: "v1", Verb: "create"})

	var verr error
	if oldText, err := c.read("old.yaml"); err == nil {
		oldJSON, err := yaml.YAMLToJSON(oldText)
		if err != nil {
			return err
		}
		old, _, err := internalObject(oldJSON)
		if err != nil {
			return err
		}
		// What the update handler has by then: the stored object's system fields, and the
		// client's object carrying the resourceVersion it read.
		// The stored object went through the create path (e.g. a Job's generated selector).
		if err := systemFields(old); err != nil {
			return err
		}
		s.PrepareForCreate(ctx, old)
		normalizeService(old)
		if err := systemFields(obj); err != nil {
			return err
		}
		normalizeService(obj)
		verr = rest.BeforeUpdate(s, withVerb(ctx, "update"), obj, old)
	} else {
		if err := systemFields(obj); err != nil {
			return err
		}
		meta, _ := meta.Accessor(obj)
		meta.SetResourceVersion("")
		normalizeService(obj)
		verr = rest.BeforeCreate(s, ctx, obj)
	}
	if verr == nil {
		return c.writeYAML("expected.yaml", []byte(`{"valid":true}`))
	}
	status, ok := verr.(apierrors.APIStatus)
	if !ok || status.Status().Details == nil || len(status.Status().Details.Causes) == 0 {
		return c.write("expected.err", []byte(verr.Error()+"\n"))
	}
	var lines []string
	for _, cause := range status.Status().Details.Causes {
		lines = append(lines, fmt.Sprintf("%s: %s", cause.Field, cause.Message))
	}
	return c.write("expected.err", []byte(strings.Join(lines, "\n")+"\n"))
}

func withVerb(ctx context.Context, verb string) context.Context {
	return genericapirequest.WithRequestInfo(ctx, &genericapirequest.RequestInfo{APIVersion: "v1", Verb: verb})
}

// normalizeService does what the Service REST handler does before validation: a single
// `clusterIP` is also the first of `clusterIPs`.
func normalizeService(obj runtime.Object) {
	if svc, ok := obj.(*api.Service); ok && len(svc.Spec.ClusterIPs) == 0 && svc.Spec.ClusterIP != "" {
		svc.Spec.ClusterIPs = []string{svc.Spec.ClusterIP}
	}
}

// systemFields fills what the API server sets on every object before validating it; the uid
// is fixed so that what depends on it (a Job's generated selector) is the same every run.
func systemFields(obj runtime.Object) error {
	accessor, err := meta.Accessor(obj)
	if err != nil {
		return err
	}
	rest.FillObjectMetaSystemFields(accessor)
	accessor.SetUID("6f0f5c2e-0000-4000-8000-000000000001")
	accessor.SetResourceVersion("1")
	return nil
}
