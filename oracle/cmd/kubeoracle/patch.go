package main

import (
	"k8s.io/apimachinery/pkg/util/strategicpatch"
)

// patchCase: `live.yaml` (a whole object) patched with `patch.yaml` as a strategic merge patch,
// the way the API server applies `kubectl patch` (the default `--type strategic`).
// Writes `expected.yaml`, or `expected.err` with upstream's error text.
func patchCase(c *caseDir) error {
	live, err := c.readJSON("live.yaml")
	if err != nil {
		return err
	}
	patch, err := c.readJSON("patch.yaml")
	if err != nil {
		return err
	}
	obj, err := typedObject(live)
	if err != nil {
		return err
	}
	merged, err := strategicpatch.StrategicMergePatch(live, patch, obj)
	if err != nil {
		return c.writeErr(err)
	}
	return c.writeYAML("expected.yaml", merged)
}
