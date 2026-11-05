package main

import (
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"testing"

	"github.com/egoist/mygo"
)

func setupBridge(t *testing.T) (*Bridge, string) {
	t.Helper()
	dir := t.TempDir()
	mygo.App.SetPath(mygo.PathUserData, dir)
	t.Cleanup(func() { mygo.App.SetPath(mygo.PathUserData, "") })
	return &Bridge{}, filepath.Join(dir, "config.ini")
}

func TestRenameKeepsExistingUserDataAndPrefersNewDirectory(t *testing.T) {
	dir := t.TempDir()
	legacy := filepath.Join(dir, "Chaoxing")
	if err := os.Mkdir(legacy, 0o700); err != nil {
		t.Fatal(err)
	}
	if err := preserveUserDataAt(dir); err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { mygo.App.SetPath(mygo.PathUserData, "") })
	if got, err := mygo.App.Path(mygo.PathUserData); err != nil || got != legacy {
		t.Fatalf("legacy user data = %q, %v", got, err)
	}
	current := filepath.Join(dir, "fkChaoxing")
	if err := os.Mkdir(current, 0o700); err != nil {
		t.Fatal(err)
	}
	mygo.App.SetPath(mygo.PathUserData, current)
	if err := preserveUserDataAt(dir); err != nil {
		t.Fatal(err)
	}
	if got, err := mygo.App.Path(mygo.PathUserData); err != nil || got != current {
		t.Fatalf("new user data = %q, %v", got, err)
	}
}

func TestInitializeCreatesPrivateConfigAndSetupStateFindsIt(t *testing.T) {
	bridge, path := setupBridge(t)
	state, err := bridge.SetupState()
	if err != nil || state.Exists || state.ConfigPath != path {
		t.Fatalf("initial state = %+v, %v", state, err)
	}

	got, err := bridge.Initialize(SetupRequest{
		Username: "13800000000", Password: "p%word", CourseIDs: "123, 456",
		Speed: 1.5, Jobs: 2, WorkEnabled: true, WorkPrefetchEnabled: true, Provider: "AI", Endpoint: "https://api.example.test/v1",
		APIKey: "secret-key", Model: "test-model", Submit: false, CoverRate: 0.9, CaptchaEnabled: true,
	})
	if err != nil || got != path {
		t.Fatalf("Initialize = %q, %v", got, err)
	}
	data, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	for _, want := range []string{
		"[common]", "username = 13800000000", "password = p%word",
		"course_list = 123, 456", "speed = 1.5", "jobs = 2",
		"work_enabled = true", "work_prefetch_enabled = true",
		"[tiku]", "provider = AI", "endpoint = https://api.example.test/v1",
		"key = secret-key", "model = test-model", "submit = false", "cover_rate = 0.9",
		"[captcha]", "enabled = true",
	} {
		if !strings.Contains(string(data), want) {
			t.Errorf("config missing %q", want)
		}
	}
	if runtime.GOOS != "windows" {
		info, err := os.Stat(path)
		if err != nil {
			t.Fatal(err)
		}
		if info.Mode().Perm() != 0o600 {
			t.Fatalf("config mode = %v, want 0600", info.Mode().Perm())
		}
	}
	state, err = bridge.SetupState()
	if err != nil || !state.Exists {
		t.Fatalf("saved state = %+v, %v", state, err)
	}
}

func TestInitializeWritesPrimaryProviderWithAIFallback(t *testing.T) {
	bridge, path := setupBridge(t)
	_, err := bridge.Initialize(SetupRequest{
		Username: "13800000000", Password: "password", Speed: 1, Jobs: 4,
		Provider: "TikuYanxi", AIFallback: true, Tokens: "yanxi-token",
		LlmPreset: "ollama", Model: "qwen3:4b-instruct",
	})
	if err != nil {
		t.Fatal(err)
	}
	data, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	for _, want := range []string{
		"provider = TikuYanxi,AI",
		"tokens = yanxi-token",
		"llm_preset = ollama",
		"model = qwen3:4b-instruct",
	} {
		if !strings.Contains(string(data), want) {
			t.Errorf("config missing %q", want)
		}
	}
	if strings.Contains(string(data), "endpoint =") || strings.Contains(string(data), "key =") {
		t.Errorf("preset config should not write empty endpoint/key:\n%s", data)
	}
}

func TestInitializeRejectsAIFallbackWithoutModelOrEndpoint(t *testing.T) {
	bridge, _ := setupBridge(t)
	for _, request := range []SetupRequest{
		{Username: "u", Password: "p", Provider: "TikuYanxi", AIFallback: true, Tokens: "t"},
		{Username: "u", Password: "p", Provider: "TikuYanxi", AIFallback: true, Tokens: "t", Model: "m"},
	} {
		if _, err := bridge.Initialize(request); err == nil {
			t.Fatalf("request %+v should fail", request)
		}
	}
}

func TestLoadConfigSplitsAIFallback(t *testing.T) {
	bridge, path := setupBridge(t)
	content := "[common]\nusername = u\npassword = p\n[tiku]\nprovider = TikuYanxi,AI\ntokens = tok\nllm_preset = ollama\nmodel = qwen3:4b-instruct\n"
	if err := os.WriteFile(path, []byte(content), 0o600); err != nil {
		t.Fatal(err)
	}
	got, err := bridge.LoadConfig(path)
	if err != nil {
		t.Fatal(err)
	}
	if got.Provider != "TikuYanxi" || !got.AIFallback || got.LlmPreset != "ollama" || got.Model != "qwen3:4b-instruct" {
		t.Fatalf("form values = %+v", got)
	}
}

func TestUpdateConfigAddsAndRemovesAIFallback(t *testing.T) {
	bridge, path := setupBridge(t)
	original := "[common]\nusername = u\npassword = p\n[tiku]\nprovider = TikuYanxi\ntokens = tok\nendpoint = https://old.example/v1\nkey = old-key\nmodel = old-model\n"
	if err := os.WriteFile(path, []byte(original), 0o600); err != nil {
		t.Fatal(err)
	}
	request := SetupRequest{
		Username: "u", Speed: 1, Jobs: 4, Provider: "TikuYanxi", AIFallback: true,
		LlmPreset: "ollama", Model: "new-model",
	}
	if _, err := bridge.UpdateConfig(path, request); err != nil {
		t.Fatal(err)
	}
	data, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	for _, want := range []string{"provider = TikuYanxi,AI", "tokens = tok", "llm_preset = ollama", "key = old-key", "model = new-model"} {
		if !strings.Contains(string(data), want) {
			t.Errorf("updated config missing %q:\n%s", want, data)
		}
	}
	request.AIFallback = false
	request.LlmPreset = ""
	if _, err := bridge.UpdateConfig(path, request); err != nil {
		t.Fatal(err)
	}
	data, err = os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(string(data), "provider = TikuYanxi\n") {
		t.Errorf("AI fallback not removed:\n%s", data)
	}
}

func TestInitializeDoesNotOverwriteExistingConfig(t *testing.T) {
	bridge, path := setupBridge(t)
	if err := os.WriteFile(path, []byte("existing config"), 0o600); err != nil {
		t.Fatal(err)
	}
	_, err := bridge.Initialize(SetupRequest{Username: "13800000000", Password: "password"})
	if err == nil {
		t.Fatal("expected existing config error")
	}
	data, err := os.ReadFile(path)
	if err != nil || string(data) != "existing config" {
		t.Fatalf("existing config changed: %q, %v", data, err)
	}
}

func TestInitializeRejectsMissingCredentialsAndNewlines(t *testing.T) {
	bridge, path := setupBridge(t)
	for _, request := range []SetupRequest{
		{Username: "13800000000"},
		{Username: "13800000000", Password: "p\n[tiku]\nprovider = AI"},
		{Username: "13800000000", Password: "password", Provider: "AI"},
	} {
		if _, err := bridge.Initialize(request); err == nil {
			t.Fatalf("request %+v should fail", request)
		}
		if _, err := os.Stat(path); !os.IsNotExist(err) {
			t.Fatalf("invalid request created config: %v", err)
		}
	}
}

func TestSetupStateRemembersSelectedConfig(t *testing.T) {
	bridge, _ := setupBridge(t)
	external := filepath.Join(t.TempDir(), "custom.ini")
	if err := os.WriteFile(external, []byte("[common]\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	if err := rememberConfig(external); err != nil {
		t.Fatal(err)
	}
	state, err := bridge.SetupState()
	if err != nil || !state.Exists || state.ConfigPath != external {
		t.Fatalf("selected state = %+v, %v", state, err)
	}
}

func TestSetupStateFindsConfigInWorkingDirectory(t *testing.T) {
	bridge, _ := setupBridge(t)
	workingDirectory := t.TempDir()
	t.Chdir(workingDirectory)
	path := filepath.Join(workingDirectory, "config.ini")
	if err := os.WriteFile(path, []byte("[common]\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	state, err := bridge.SetupState()
	if err != nil || !state.Exists || state.ConfigPath != path {
		t.Fatalf("working directory state = %+v, %v", state, err)
	}
}

func TestInitializeSelectsNewConfigAfterExistingFile(t *testing.T) {
	bridge, defaultPath := setupBridge(t)
	external := filepath.Join(t.TempDir(), "old.ini")
	if err := os.WriteFile(external, []byte("[common]\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	if err := rememberConfig(external); err != nil {
		t.Fatal(err)
	}
	if _, err := bridge.Initialize(SetupRequest{Username: "13800000000", Password: "password"}); err != nil {
		t.Fatal(err)
	}
	state, err := bridge.SetupState()
	if err != nil || state.ConfigPath != defaultPath || !state.Exists {
		t.Fatalf("new config state = %+v, %v", state, err)
	}
}

func TestLoadConfigProvidesFormValuesWithoutSecrets(t *testing.T) {
	bridge, path := setupBridge(t)
	content := "[common]\nuse_cookies = true\nusername = 13800000000\npassword = hidden-password\ncourse_list = 123,456\nspeed = 1.5\njobs = 2\nwork_prefetch_enabled = true\n[tiku]\nprovider = AI\nendpoint = https://api.example.test/v1\nkey = hidden-key\nmodel = test-model\nsubmit = true\ncover_rate = 0.75\n[captcha]\nenabled = true\n"
	if err := os.WriteFile(path, []byte(content), 0o600); err != nil {
		t.Fatal(err)
	}
	got, err := bridge.LoadConfig(path)
	if err != nil {
		t.Fatal(err)
	}
	if got.Username != "13800000000" || got.CourseIDs != "123,456" || got.Speed != 1.5 || got.Jobs != 2 || !got.UseCookies || !got.WorkPrefetchEnabled || got.Provider != "AI" || got.Endpoint != "https://api.example.test/v1" || got.Model != "test-model" || !got.Submit || got.CoverRate != 0.75 || !got.CaptchaEnabled {
		t.Fatalf("form values = %+v", got)
	}
	if got.Password != "" || got.Tokens != "" || got.APIKey != "" {
		t.Fatal("form exposed a secret")
	}
}

func TestUpdateConfigPreservesUnknownSettingsAndBlankSecrets(t *testing.T) {
	bridge, path := setupBridge(t)
	original := "# keep comment\n[common]\nuse_cookies = false\nusername = old\npassword = private-password\nspeed = 1\njobs = 4\ncover_rate = 0.9\n[tiku]\nprovider = AI\nendpoint = https://api.example.test/v1\nkey = private-key\nmodel = old-model\nsubmit = false\n[custom]\noption = keep\n"
	if err := os.WriteFile(path, []byte(original), 0o600); err != nil {
		t.Fatal(err)
	}
	request := SetupRequest{Username: "new", CourseIDs: "123", Speed: 1.5, Jobs: 2, WorkPrefetchEnabled: true, Provider: "AI", Endpoint: "https://api.example.test/v1", Model: "new-model", Submit: true, CoverRate: 0.95}
	if _, err := bridge.UpdateConfig(path, request); err != nil {
		t.Fatal(err)
	}
	data, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	for _, want := range []string{"# keep comment", "cover_rate = 0.9", "cover_rate = 0.95", "[custom]\noption = keep", "password = private-password", "key = private-key", "username = new", "course_list = 123", "speed = 1.5", "jobs = 2", "work_prefetch_enabled = true", "model = new-model", "submit = true"} {
		if !strings.Contains(string(data), want) {
			t.Errorf("updated config missing %q", want)
		}
	}
}

func TestUpdateConfigRejectsMissingNewProviderSecretWithoutChangingFile(t *testing.T) {
	bridge, path := setupBridge(t)
	original := "[common]\nusername = user\npassword = password\n[tiku]\nprovider = TikuGo\n"
	if err := os.WriteFile(path, []byte(original), 0o600); err != nil {
		t.Fatal(err)
	}
	request := SetupRequest{Username: "user", Speed: 1, Jobs: 4, Provider: "TikuYanxi"}
	if _, err := bridge.UpdateConfig(path, request); err == nil {
		t.Fatal("expected missing token error")
	}
	data, err := os.ReadFile(path)
	if err != nil || string(data) != original {
		t.Fatalf("config changed after invalid update: %v", err)
	}
}

func TestUpdateConfigKeepsCookieOnlyLoginAndUnknownProvider(t *testing.T) {
	bridge, path := setupBridge(t)
	original := "[common]\nuse_cookies = true\nspeed = 1\njobs = 4\n[tiku]\nprovider = SiliconFlow\nkey = private-key\n"
	if err := os.WriteFile(path, []byte(original), 0o600); err != nil {
		t.Fatal(err)
	}
	request := SetupRequest{UseCookies: true, Speed: 1.2, Jobs: 3, Provider: "SiliconFlow"}
	if _, err := bridge.UpdateConfig(path, request); err != nil {
		t.Fatal(err)
	}
	data, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	for _, want := range []string{"use_cookies = true", "provider = SiliconFlow", "key = private-key", "speed = 1.2", "jobs = 3"} {
		if !strings.Contains(string(data), want) {
			t.Errorf("updated config missing %q", want)
		}
	}
}

func TestUpdateConfigAppliesDefaultSpeedAndJobs(t *testing.T) {
	bridge, path := setupBridge(t)
	if err := os.WriteFile(path, []byte("[common]\nusername = user\npassword = password\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	if _, err := bridge.UpdateConfig(path, SetupRequest{Username: "user"}); err != nil {
		t.Fatal(err)
	}
	data, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(string(data), "speed = 1\n") || !strings.Contains(string(data), "jobs = 4\n") || !strings.Contains(string(data), "cover_rate = 0.8\n") {
		t.Fatalf("defaults not written: %q", data)
	}
}

func TestInitializeRejectsInvalidCoverRate(t *testing.T) {
	bridge, _ := setupBridge(t)
	for _, rate := range []float64{-0.1, 1.1} {
		if _, err := bridge.Initialize(SetupRequest{Username: "u", Password: "p", CoverRate: rate}); err == nil {
			t.Fatalf("cover rate %v should fail", rate)
		}
	}
}

func TestLoadConfigFallsBackForInvalidCoverRate(t *testing.T) {
	bridge, path := setupBridge(t)
	content := "[common]\nusername = u\npassword = p\n[tiku]\nprovider = TikuGo\ncover_rate = 1.5\n"
	if err := os.WriteFile(path, []byte(content), 0o600); err != nil {
		t.Fatal(err)
	}
	got, err := bridge.LoadConfig(path)
	if err != nil || got.CoverRate != defaultCoverRate {
		t.Fatalf("invalid cover rate fallback = %+v, %v", got, err)
	}
}

func TestUpdateConfigRejectsInvalidCoverRateWithoutChangingFile(t *testing.T) {
	bridge, path := setupBridge(t)
	original := "[common]\nusername = user\npassword = password\n[tiku]\nprovider = TikuGo\ncover_rate = 0.8\n"
	if err := os.WriteFile(path, []byte(original), 0o600); err != nil {
		t.Fatal(err)
	}
	if _, err := bridge.UpdateConfig(path, SetupRequest{Username: "user", CoverRate: 1.2}); err == nil {
		t.Fatal("expected invalid cover rate error")
	}
	data, err := os.ReadFile(path)
	if err != nil || string(data) != original {
		t.Fatalf("config changed after invalid update: %v", err)
	}
}

func TestWorkEnabledLoadsLegacyDefaultAndPersistsBothValues(t *testing.T) {
	bridge, path := setupBridge(t)
	if err := os.WriteFile(path, []byte("[common]\nusername = user\npassword = password\n[tiku]\nprovider = TikuGo\nsubmit = true\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	request, err := bridge.LoadConfig(path)
	if err != nil || !request.WorkEnabled || request.CoverRate != defaultCoverRate {
		t.Fatalf("legacy default = %+v, %v", request, err)
	}
	for _, enabled := range []bool{false, true} {
		request.WorkEnabled = enabled
		if _, err := bridge.UpdateConfig(path, request); err != nil {
			t.Fatal(err)
		}
		got, err := bridge.LoadConfig(path)
		if err != nil || got.WorkEnabled != enabled || got.Provider != "TikuGo" || !got.Submit {
			t.Fatalf("reloaded config = %+v, %v", got, err)
		}
		newRequest := request
		newRequest.Password = "password"
		content, err := newRequest.configText()
		if err != nil || !strings.Contains(content, "work_enabled = "+map[bool]string{true: "true", false: "false"}[enabled]+"\n") {
			t.Fatalf("new config did not preserve switch: %v", err)
		}
	}
}

func TestWorkPrefetchLoadsLegacyDefaultAndPersists(t *testing.T) {
	bridge, path := setupBridge(t)
	if err := os.WriteFile(path, []byte("[common]\nusername = user\npassword = password\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	request, err := bridge.LoadConfig(path)
	if err != nil || request.WorkPrefetchEnabled {
		t.Fatalf("legacy prefetch default = %+v, %v", request, err)
	}
	request.WorkPrefetchEnabled = true
	if _, err := bridge.UpdateConfig(path, request); err != nil {
		t.Fatal(err)
	}
	got, err := bridge.LoadConfig(path)
	if err != nil || !got.WorkPrefetchEnabled {
		t.Fatalf("reloaded prefetch = %+v, %v", got, err)
	}
}
