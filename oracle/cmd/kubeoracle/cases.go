package main

import (
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"strings"

	"k8s.io/apimachinery/pkg/runtime"
	"k8s.io/apimachinery/pkg/runtime/schema"
	"k8s.io/client-go/kubernetes/scheme"
	"sigs.k8s.io/yaml"
)

// An area turns one case directory's inputs into its expected outputs.
type area func(c *caseDir) error

var areas = map[string]area{
	"patch": patchCase,
}

// goldens runs every case of every area under root (`<root>/<area>/<case>/`). Expected files
// a case no longer produces are removed, so a case that used to fail and now succeeds does not
// keep a stale `expected.err`.
func goldens(root string) error {
	names := make([]string, 0, len(areas))
	for name := range areas {
		names = append(names, name)
	}
	sort.Strings(names)
	for _, name := range names {
		dirs, err := os.ReadDir(filepath.Join(root, name))
		if errors.Is(err, os.ErrNotExist) {
			continue
		}
		if err != nil {
			return err
		}
		for _, d := range dirs {
			if !d.IsDir() {
				continue
			}
			c := &caseDir{path: filepath.Join(root, name, d.Name())}
			if err := c.clearExpected(); err != nil {
				return err
			}
			if err := areas[name](c); err != nil {
				return fmt.Errorf("%s: %w", c.path, err)
			}
		}
	}
	return nil
}

type caseDir struct{ path string }

func (c *caseDir) read(name string) ([]byte, error) {
	return os.ReadFile(filepath.Join(c.path, name))
}

// readJSON reads a YAML input as JSON, the form upstream's patch code takes.
func (c *caseDir) readJSON(name string) ([]byte, error) {
	text, err := c.read(name)
	if err != nil {
		return nil, err
	}
	return yaml.YAMLToJSON(text)
}

func (c *caseDir) write(name string, data []byte) error {
	return os.WriteFile(filepath.Join(c.path, name), data, 0o644)
}

// writeYAML writes JSON as YAML with sorted keys; the Rust side compares values, not text.
func (c *caseDir) writeYAML(name string, json []byte) error {
	text, err := yaml.JSONToYAML(json)
	if err != nil {
		return err
	}
	return c.write(name, text)
}

// writeErr records an upstream error as the case's expected outcome.
func (c *caseDir) writeErr(err error) error {
	return c.write("expected.err", []byte(err.Error()+"\n"))
}

func (c *caseDir) clearExpected() error {
	entries, err := os.ReadDir(c.path)
	if err != nil {
		return err
	}
	for _, e := range entries {
		if strings.HasPrefix(e.Name(), "expected.") {
			if err := os.Remove(filepath.Join(c.path, e.Name())); err != nil {
				return err
			}
		}
	}
	return nil
}

// typedObject is an empty object of the Go type upstream uses for a manifest's apiVersion and
// kind; strategic merge patch reads its struct tags.
func typedObject(json []byte) (runtime.Object, error) {
	var meta struct {
		APIVersion string `json:"apiVersion"`
		Kind       string `json:"kind"`
	}
	if err := yaml.Unmarshal(json, &meta); err != nil {
		return nil, err
	}
	gv, err := schema.ParseGroupVersion(meta.APIVersion)
	if err != nil {
		return nil, err
	}
	return scheme.Scheme.New(gv.WithKind(meta.Kind))
}
