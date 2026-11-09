package main

import (
	"encoding/json"
	"fmt"

	"k8s.io/apimachinery/pkg/runtime/schema"
	"k8s.io/kubernetes/pkg/api/legacyscheme"
	"sigs.k8s.io/yaml"

	// Registers the groups' types and their defaulting functions in legacyscheme.
	_ "k8s.io/kubernetes/pkg/apis/apps/install"
	_ "k8s.io/kubernetes/pkg/apis/batch/install"
	_ "k8s.io/kubernetes/pkg/apis/core/install"
	_ "k8s.io/kubernetes/pkg/apis/discovery/install"
	_ "k8s.io/kubernetes/pkg/apis/networking/install"
)

// defaultsCase: `manifest.yaml` decoded into its versioned Go type and defaulted with the
// scheme's defaulters (what the API server's decoder does to every object it reads), then
// encoded again. Only defaulting: no create strategy, no admission. `status` is left out (the
// engine's controllers own it). Writes `expected.yaml`.
func defaultsCase(c *caseDir) error {
	manifest, err := c.readJSON("manifest.yaml")
	if err != nil {
		return err
	}
	var meta struct {
		APIVersion string `json:"apiVersion"`
		Kind       string `json:"kind"`
	}
	if err := yaml.Unmarshal(manifest, &meta); err != nil {
		return err
	}
	gv, err := schema.ParseGroupVersion(meta.APIVersion)
	if err != nil {
		return err
	}
	obj, err := legacyscheme.Scheme.New(gv.WithKind(meta.Kind))
	if err != nil {
		return err
	}
	if err := json.Unmarshal(manifest, obj); err != nil {
		return fmt.Errorf("decoding manifest: %w", err)
	}
	legacyscheme.Scheme.Default(obj)
	out, err := json.Marshal(obj)
	if err != nil {
		return err
	}
	var fields map[string]any
	if err := json.Unmarshal(out, &fields); err != nil {
		return err
	}
	delete(fields, "status")
	out, err = json.Marshal(fields)
	if err != nil {
		return err
	}
	return c.writeYAML("expected.yaml", out)
}
