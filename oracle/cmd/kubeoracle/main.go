// Command kubeoracle runs upstream Kubernetes code on the engine's fidelity cases and writes what
// upstream produces next to each case's inputs, as the expected output the Rust engine is tested
// against (`engine/tests/fidelity.rs`). It never ships: the engine stays pure Rust, and upstream
// is only the reference. See docs/fidelity.md.
//
//	kubeoracle goldens <fidelity dir>   write expected.* for every case of every area
//	kubeoracle patchmeta <file.rs>      write the engine's strategic merge patch metadata
package main

import (
	"fmt"
	"os"
)

func main() {
	if err := run(os.Args[1:]); err != nil {
		fmt.Fprintln(os.Stderr, "kubeoracle:", err)
		os.Exit(1)
	}
}

func run(args []string) error {
	if len(args) != 2 {
		return fmt.Errorf("usage: kubeoracle goldens <fidelity dir> | kubeoracle patchmeta <file.rs>")
	}
	switch args[0] {
	case "goldens":
		return goldens(args[1])
	case "patchmeta":
		return patchmeta(args[1])
	default:
		return fmt.Errorf("unknown command %q", args[0])
	}
}
