package main

import (
	"bytes"
	"encoding/json"
	"flag"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"time"
)

type fixture struct {
	CapturedAt      string                 `json:"captured_at"`
	LegacyRoot      string                 `json:"legacy_root"`
	BeadsFile       string                 `json:"beads_file"`
	DiffSince       string                 `json:"diff_since"`
	Triage          map[string]interface{} `json:"triage"`
	Plan            map[string]interface{} `json:"plan"`
	Insights        map[string]interface{} `json:"insights"`
	Priority        map[string]interface{} `json:"priority"`
	Forecast        map[string]interface{} `json:"forecast"`
	Capacity        map[string]interface{} `json:"capacity"`
	CapacityByLabel map[string]interface{} `json:"capacity_by_label"`
	History         map[string]interface{} `json:"history"`
	Diff            map[string]interface{} `json:"diff"`
}

func main() {
	var legacyRoot string
	var beadsFile string
	var diffBeforeFile string
	var outputPath string

	flag.StringVar(&legacyRoot, "legacy-root", "", "Path to legacy beads_viewer repo root")
	flag.StringVar(&beadsFile, "beads-file", "", "Path to JSONL fixture file")
	flag.StringVar(&diffBeforeFile, "diff-before-file", "", "Path to JSONL snapshot to commit before beads-file")
	flag.StringVar(&outputPath, "output", "", "Path to output fixture JSON")
	flag.Parse()

	if legacyRoot == "" || beadsFile == "" || outputPath == "" {
		fmt.Fprintln(os.Stderr, "usage: main --legacy-root <path> --beads-file <path> --output <path>")
		os.Exit(2)
	}

	tempDir, err := os.MkdirTemp("", "bvr-go-ref-*")
	must(err)
	defer os.RemoveAll(tempDir)

	beadsDir := filepath.Join(tempDir, ".beads")
	must(os.MkdirAll(beadsDir, 0o755))
	repoDir := tempDir

	beforePath := beadsFile
	if diffBeforeFile != "" {
		beforePath = diffBeforeFile
	}

	beforeData, err := os.ReadFile(beforePath)
	must(err)
	afterData, err := os.ReadFile(beadsFile)
	must(err)

	must(os.WriteFile(filepath.Join(beadsDir, "beads.jsonl"), beforeData, 0o644))

	run(repoDir, "git", "init")
	run(repoDir, "git", "config", "user.email", "conformance@example.com")
	run(repoDir, "git", "config", "user.name", "Conformance Bot")
	run(repoDir, "git", "add", ".")
	run(repoDir, "git", "commit", "-m", "seed snapshot")

	must(os.WriteFile(filepath.Join(beadsDir, "beads.jsonl"), afterData, 0o644))
	run(repoDir, "git", "add", ".")
	run(repoDir, "git", "commit", "--allow-empty", "-m", "update snapshot")

	legacyBin := filepath.Join(tempDir, "bv-legacy")
	run(legacyRoot, "go", "build", "-o", legacyBin, "./cmd/bv")

	triage := runLegacyJSON(legacyBin, repoDir, beadsDir, "--robot-triage")
	plan := runLegacyJSON(legacyBin, repoDir, beadsDir, "--robot-plan")
	insights := runLegacyJSON(legacyBin, repoDir, beadsDir, "--robot-insights")
	priority := runLegacyJSON(legacyBin, repoDir, beadsDir, "--robot-priority", "--robot-max-results", "10")
	forecast := runLegacyJSON(legacyBin, repoDir, beadsDir, "--robot-forecast", "all", "--forecast-agents", "2")
	capacity := runLegacyJSON(legacyBin, repoDir, beadsDir, "--robot-capacity", "--agents", "3")
	capacityByLabel := runLegacyJSON(legacyBin, repoDir, beadsDir, "--robot-capacity", "--capacity-label", "backend", "--agents", "1")
	history := runLegacyJSON(legacyBin, repoDir, beadsDir, "--robot-history", "--history-limit", "20")
	diff := runLegacyJSON(legacyBin, repoDir, beadsDir, "--robot-diff", "--diff-since", "HEAD~1")

	fx := fixture{
		CapturedAt:      time.Now().UTC().Format(time.RFC3339),
		LegacyRoot:      legacyRoot,
		BeadsFile:       beadsFile,
		DiffSince:       "HEAD~1",
		Triage:          triage,
		Plan:            plan,
		Insights:        insights,
		Priority:        priority,
		Forecast:        forecast,
		Capacity:        capacity,
		CapacityByLabel: capacityByLabel,
		History:         history,
		Diff:            diff,
	}

	encoded, err := json.MarshalIndent(fx, "", "  ")
	must(err)
	must(os.WriteFile(outputPath, encoded, 0o644))
}

func runLegacyJSON(binaryPath string, repoDir string, beadsDir string, args ...string) map[string]interface{} {
	baseArgs := []string{"--format", "json"}
	baseArgs = append(baseArgs, args...)

	cmd := exec.Command(binaryPath, baseArgs...)
	cmd.Dir = repoDir
	cmd.Env = append(os.Environ(), "BV_ROBOT=1", "BEADS_DIR="+beadsDir)

	var stdout bytes.Buffer
	var stderr bytes.Buffer
	cmd.Stdout = &stdout
	cmd.Stderr = &stderr

	if err := cmd.Run(); err != nil {
		fmt.Fprintf(os.Stderr, "legacy command failed: %v\nargs=%v\nstderr=%s\n", err, args, stderr.String())
		os.Exit(1)
	}

	var payload map[string]interface{}
	if err := json.Unmarshal(stdout.Bytes(), &payload); err != nil {
		fmt.Fprintf(os.Stderr, "failed to decode legacy JSON for args=%v: %v\nstdout=%s\n", args, err, stdout.String())
		os.Exit(1)
	}

	return payload
}

func run(dir string, name string, args ...string) {
	cmd := exec.Command(name, args...)
	cmd.Dir = dir
	out, err := cmd.CombinedOutput()
	if err != nil {
		fmt.Fprintf(os.Stderr, "command failed: %s %v\n%s\n", name, args, string(out))
		os.Exit(1)
	}
}

func must(err error) {
	if err != nil {
		panic(err)
	}
}
