package main

import (
	"encoding/json"
	"errors"
	"fmt"
	"math"
	"os"
	"path/filepath"
	"strconv"
	"strings"

	"github.com/egoist/mygo"
)

type SetupState struct {
	ConfigPath string `json:"configPath"`
	Exists     bool   `json:"exists"`
}

const defaultCoverRate = 0.8

type SetupRequest struct {
	UseCookies          bool    `json:"useCookies"`
	Username            string  `json:"username"`
	Password            string  `json:"password"`
	CourseIDs           string  `json:"courseIds"`
	Speed               float64 `json:"speed"`
	Jobs                int     `json:"jobs"`
	WorkEnabled         bool    `json:"workEnabled"`
	WorkPrefetchEnabled bool    `json:"workPrefetchEnabled"`
	CaptchaEnabled      bool    `json:"captchaEnabled"`
	Provider            string  `json:"provider"`
	AIFallback          bool    `json:"aiFallback"`
	Tokens              string  `json:"tokens"`
	GoAuthorization     string  `json:"goAuthorization"`
	LlmPreset           string  `json:"llmPreset"`
	Endpoint            string  `json:"endpoint"`
	APIKey              string  `json:"apiKey"`
	Model               string  `json:"model"`
	Submit              bool    `json:"submit"`
	CoverRate           float64 `json:"coverRate"`
}

func setupConfigPath() (string, error) {
	dir, err := mygo.App.Path(mygo.PathUserData)
	if err != nil {
		return "", fmt.Errorf("获取应用数据目录失败：%w", err)
	}
	return filepath.Join(dir, "config.ini"), nil
}

func selectedConfigPath() (string, error) {
	dir, err := mygo.App.Path(mygo.PathUserData)
	if err != nil {
		return "", fmt.Errorf("获取应用数据目录失败：%w", err)
	}
	return filepath.Join(dir, "selected-config.json"), nil
}

func rememberConfig(path string) error {
	absolute, err := filepath.Abs(path)
	if err != nil {
		return fmt.Errorf("解析配置文件路径失败：%w", err)
	}
	info, err := os.Stat(absolute)
	if err != nil || info.IsDir() {
		return errors.New("选中的配置文件不存在或不是文件")
	}
	selection, err := selectedConfigPath()
	if err != nil {
		return err
	}
	data, err := json.Marshal(absolute)
	if err != nil {
		return fmt.Errorf("保存配置路径失败：%w", err)
	}
	if err := os.WriteFile(selection, data, 0o600); err != nil {
		return fmt.Errorf("保存配置路径失败：%w", err)
	}
	return nil
}

func (b *Bridge) SetupState() (SetupState, error) {
	path, err := setupConfigPath()
	if err != nil {
		return SetupState{}, err
	}
	selection, err := selectedConfigPath()
	if err != nil {
		return SetupState{}, err
	}
	if data, err := os.ReadFile(selection); err == nil {
		var selected string
		if json.Unmarshal(data, &selected) == nil {
			if info, err := os.Stat(selected); err == nil && !info.IsDir() {
				return SetupState{ConfigPath: selected, Exists: true}, nil
			}
		}
	} else if !errors.Is(err, os.ErrNotExist) {
		return SetupState{}, fmt.Errorf("读取上次选择的配置失败：%w", err)
	}
	_, err = os.Stat(path)
	if err != nil && !errors.Is(err, os.ErrNotExist) {
		return SetupState{}, fmt.Errorf("检查配置文件失败：%w", err)
	}
	if err == nil {
		return SetupState{ConfigPath: path, Exists: true}, nil
	}
	workingConfig, err := filepath.Abs("config.ini")
	if err != nil {
		return SetupState{}, fmt.Errorf("解析启动目录配置路径失败：%w", err)
	}
	if info, err := os.Stat(workingConfig); err == nil && !info.IsDir() {
		return SetupState{ConfigPath: workingConfig, Exists: true}, nil
	} else if err != nil && !errors.Is(err, os.ErrNotExist) {
		return SetupState{}, fmt.Errorf("检查启动目录配置失败：%w", err)
	}
	return SetupState{ConfigPath: path, Exists: false}, nil
}

func (b *Bridge) Initialize(request SetupRequest) (string, error) {
	content, err := request.configText()
	if err != nil {
		return "", err
	}
	path, err := setupConfigPath()
	if err != nil {
		return "", err
	}
	file, err := os.OpenFile(path, os.O_WRONLY|os.O_CREATE|os.O_EXCL, 0o600)
	if errors.Is(err, os.ErrExist) {
		return "", errors.New("配置文件已经存在；请直接选择已有配置")
	}
	if err != nil {
		return "", fmt.Errorf("创建配置文件失败：%w", err)
	}
	if _, err = file.WriteString(content); err != nil {
		file.Close()
		os.Remove(path)
		return "", fmt.Errorf("写入配置文件失败：%w", err)
	}
	if err = file.Close(); err != nil {
		os.Remove(path)
		return "", fmt.Errorf("关闭配置文件失败：%w", err)
	}
	if err = rememberConfig(path); err != nil {
		os.Remove(path)
		return "", err
	}
	return path, nil
}

func splitProviderChain(value string) []string {
	var out []string
	for _, item := range strings.Split(value, ",") {
		item = strings.TrimSpace(item)
		if item != "" {
			out = append(out, item)
		}
	}
	return out
}

func isAIProvider(value string) bool {
	return value == "AI" || value == "LLM"
}

func providerChainHas(chain, name string) bool {
	for _, provider := range splitProviderChain(chain) {
		if provider == name {
			return true
		}
	}
	return false
}

// providerChain 把“主题库 + 可选 AI 回退”转换成写入 config.ini 的 provider 值。
func (request SetupRequest) providerChain() string {
	primary := strings.TrimSpace(request.Provider)
	ai := request.AIFallback || isAIProvider(primary) || providerChainHas(primary, "AI") || providerChainHas(primary, "LLM")
	var kept []string
	for _, provider := range splitProviderChain(primary) {
		if !isAIProvider(provider) {
			kept = append(kept, provider)
		}
	}
	chain := strings.Join(kept, ",")
	if !ai {
		return chain
	}
	if chain == "" {
		return "AI"
	}
	return chain + ",AI"
}

// splitProviderConfig 把 config.ini 中的 provider 拆成“主题库 + 是否启用 AI 回退”。
func splitProviderConfig(chain string) (string, bool) {
	var kept []string
	ai := false
	for _, provider := range splitProviderChain(chain) {
		if isAIProvider(provider) {
			ai = true
			continue
		}
		kept = append(kept, provider)
	}
	if len(kept) == 0 && ai {
		return "AI", true
	}
	return strings.Join(kept, ","), ai
}

func (request SetupRequest) configText() (string, error) {
	request.Username = strings.TrimSpace(request.Username)
	request.CourseIDs = strings.TrimSpace(request.CourseIDs)
	request.Provider = strings.TrimSpace(request.Provider)
	request.Tokens = strings.TrimSpace(request.Tokens)
	request.GoAuthorization = strings.TrimSpace(request.GoAuthorization)
	request.LlmPreset = strings.TrimSpace(request.LlmPreset)
	request.Endpoint = strings.TrimSpace(request.Endpoint)
	request.Model = strings.TrimSpace(request.Model)
	if !request.UseCookies && (request.Username == "" || request.Password == "") {
		return "", errors.New("请填写登录账号和密码")
	}
	if strings.TrimSpace(request.Password) != request.Password || strings.TrimSpace(request.APIKey) != request.APIKey {
		return "", errors.New("密码和 API Key 的首尾不能有空白字符")
	}
	fields := map[string]string{
		"账号": request.Username, "密码": request.Password, "课程 ID": request.CourseIDs,
		"题库": request.Provider, "题库 Token": request.Tokens,
		"GO 题授权": request.GoAuthorization, "AI 服务商预设": request.LlmPreset,
		"API 地址": request.Endpoint, "API Key": request.APIKey, "模型": request.Model,
	}
	for name, value := range fields {
		if strings.ContainsAny(value, "\r\n") {
			return "", fmt.Errorf("%s 不能包含换行", name)
		}
	}
	if request.Speed == 0 {
		request.Speed = 1
	}
	if request.Jobs == 0 {
		request.Jobs = 4
	}
	if math.IsNaN(request.Speed) || math.IsInf(request.Speed, 0) || request.Speed < 1 || request.Speed > 2 {
		return "", errors.New("播放倍速须在 1 到 2 之间")
	}
	if request.Jobs < 1 || request.Jobs > 16 {
		return "", errors.New("并发章节数须在 1 到 16 之间")
	}
	if request.CoverRate == 0 {
		request.CoverRate = defaultCoverRate
	}
	if math.IsNaN(request.CoverRate) || math.IsInf(request.CoverRate, 0) || request.CoverRate < 0 || request.CoverRate > 1 {
		return "", errors.New("自动提交覆盖率须在 0 到 1 之间")
	}
	chain := request.providerChain()
	for _, provider := range splitProviderChain(chain) {
		if !knownTikuProvider(provider) {
			return "", fmt.Errorf("不支持的题库类型: %s", provider)
		}
	}
	hasYanxi := providerChainHas(chain, "TikuYanxi")
	hasGo := providerChainHas(chain, "TikuGo")
	aiEnabled := providerChainHas(chain, "AI")
	if hasYanxi && request.Tokens == "" {
		return "", errors.New("言溪题库需要 Token")
	}
	if aiEnabled {
		if request.Model == "" {
			return "", errors.New("AI 回退需要模型名")
		}
		if request.LlmPreset == "" && request.Endpoint == "" {
			return "", errors.New("AI 回退需要 API 地址，或填写服务商预设")
		}
	}
	var out strings.Builder
	out.WriteString("[common]\n")
	writeINI(&out, "use_cookies", strconv.FormatBool(request.UseCookies))
	writeINI(&out, "username", request.Username)
	writeINI(&out, "password", request.Password)
	writeINI(&out, "course_list", request.CourseIDs)
	writeINI(&out, "speed", strconv.FormatFloat(request.Speed, 'f', -1, 64))
	writeINI(&out, "jobs", strconv.Itoa(request.Jobs))
	writeINI(&out, "work_enabled", strconv.FormatBool(request.WorkEnabled))
	writeINI(&out, "work_prefetch_enabled", strconv.FormatBool(request.WorkPrefetchEnabled))
	out.WriteString("\n[tiku]\n")
	writeINI(&out, "provider", chain)
	writeINI(&out, "submit", strconv.FormatBool(request.Submit))
	writeINI(&out, "cover_rate", strconv.FormatFloat(request.CoverRate, 'f', -1, 64))
	if hasYanxi {
		writeINI(&out, "tokens", request.Tokens)
	}
	if hasGo && request.GoAuthorization != "" {
		writeINI(&out, "go_authorization", request.GoAuthorization)
	}
	if aiEnabled {
		if request.LlmPreset != "" {
			writeINI(&out, "llm_preset", request.LlmPreset)
		}
		if request.Endpoint != "" {
			writeINI(&out, "endpoint", request.Endpoint)
		}
		if request.APIKey != "" {
			writeINI(&out, "key", request.APIKey)
		}
		writeINI(&out, "model", request.Model)
	}
	out.WriteString("\n[captcha]\n")
	writeINI(&out, "enabled", strconv.FormatBool(request.CaptchaEnabled))
	return out.String(), nil
}

func writeINI(out *strings.Builder, key, value string) {
	out.WriteString(key)
	out.WriteString(" = ")
	out.WriteString(value)
	out.WriteByte('\n')
}

type iniUpdate struct{ section, key, value string }

func configFilePath(path string) (string, error) {
	if strings.TrimSpace(path) == "" {
		return setupConfigPath()
	}
	absolute, err := filepath.Abs(strings.TrimSpace(path))
	if err != nil {
		return "", fmt.Errorf("解析配置文件路径失败：%w", err)
	}
	return absolute, nil
}

func readConfigFile(path string) (string, string, map[string]map[string]string, error) {
	absolute, err := configFilePath(path)
	if err != nil {
		return "", "", nil, err
	}
	data, err := os.ReadFile(absolute)
	if err != nil {
		return "", "", nil, fmt.Errorf("读取配置文件失败：%w", err)
	}
	content := string(data)
	values := make(map[string]map[string]string)
	section := ""
	for _, line := range strings.Split(strings.TrimPrefix(content, "\ufeff"), "\n") {
		trimmed := strings.TrimSpace(line)
		if strings.HasPrefix(trimmed, "[") && strings.HasSuffix(trimmed, "]") {
			section = strings.TrimSpace(trimmed[1 : len(trimmed)-1])
			if values[section] == nil {
				values[section] = make(map[string]string)
			}
		} else if section != "" && !strings.HasPrefix(trimmed, "#") && !strings.HasPrefix(trimmed, ";") {
			if key, value, ok := strings.Cut(trimmed, "="); ok && strings.TrimSpace(key) != "" {
				values[section][strings.TrimSpace(key)] = strings.TrimSpace(value)
			}
		}
	}
	if values["common"] == nil {
		return "", "", nil, errors.New("配置文件缺少 [common] 节")
	}
	return absolute, content, values, nil
}

func configValue(values map[string]map[string]string, section, key string) string {
	return values[section][key]
}

func configBool(value string) bool {
	switch strings.ToLower(strings.TrimSpace(value)) {
	case "1", "true", "yes", "y", "on":
		return true
	}
	return false
}

// LoadConfig returns editable values. Credentials stay in the Go process.
func (b *Bridge) LoadConfig(path string) (SetupRequest, error) {
	_, _, values, err := readConfigFile(path)
	if err != nil {
		return SetupRequest{}, err
	}
	speed, _ := strconv.ParseFloat(configValue(values, "common", "speed"), 64)
	if speed == 0 {
		speed = 1
	}
	jobs, _ := strconv.Atoi(configValue(values, "common", "jobs"))
	if jobs == 0 {
		jobs = 4
	}
	coverRate, _ := strconv.ParseFloat(configValue(values, "tiku", "cover_rate"), 64)
	if coverRate == 0 || math.IsNaN(coverRate) || math.IsInf(coverRate, 0) || coverRate < 0 || coverRate > 1 {
		coverRate = defaultCoverRate
	}
	provider, aiFallback := splitProviderConfig(configValue(values, "tiku", "provider"))
	return SetupRequest{
		UseCookies: configBool(configValue(values, "common", "use_cookies")),
		Username:   configValue(values, "common", "username"),
		CourseIDs:  configValue(values, "common", "course_list"),
		Speed:      speed, Jobs: jobs,
		WorkEnabled:         strings.TrimSpace(configValue(values, "common", "work_enabled")) == "" || configBool(configValue(values, "common", "work_enabled")),
		WorkPrefetchEnabled: configBool(configValue(values, "common", "work_prefetch_enabled")),
		CaptchaEnabled:      configBool(configValue(values, "captcha", "enabled")),
		Provider:            provider,
		AIFallback:          aiFallback,
		LlmPreset:           configValue(values, "tiku", "llm_preset"),
		Endpoint:            configValue(values, "tiku", "endpoint"),
		Model:               configValue(values, "tiku", "model"),
		Submit:              configBool(configValue(values, "tiku", "submit")),
		CoverRate:           coverRate,
	}, nil
}

// UpdateConfig changes only fields represented by the form, preserving comments and other settings.
func (b *Bridge) UpdateConfig(path string, request SetupRequest) (string, error) {
	absolute, content, values, err := readConfigFile(path)
	if err != nil {
		return "", err
	}
	if request.Speed == 0 {
		request.Speed = 1
	}
	if request.Jobs == 0 {
		request.Jobs = 4
	}
	if request.CoverRate == 0 {
		request.CoverRate = defaultCoverRate
	}
	if request.Password == "" {
		request.Password = configValue(values, "common", "password")
	}
	if request.Tokens == "" {
		request.Tokens = configValue(values, "tiku", "tokens")
	}
	if request.GoAuthorization == "" {
		request.GoAuthorization = configValue(values, "tiku", "go_authorization")
	}
	if request.APIKey == "" {
		request.APIKey = configValue(values, "tiku", "key")
	}
	chain := request.providerChain()
	oldProvider := configValue(values, "tiku", "provider")
	// Existing provider types outside the wizard remain editable without losing their settings.
	validate := request
	if chain == oldProvider && !knownProviderChain(chain) {
		validate.Provider = ""
		validate.AIFallback = false
	}
	if _, err := validate.configText(); err != nil {
		return "", err
	}
	updates := []iniUpdate{
		{"common", "use_cookies", strconv.FormatBool(request.UseCookies)},
		{"common", "username", strings.TrimSpace(request.Username)},
		{"common", "password", request.Password},
		{"common", "course_list", strings.TrimSpace(request.CourseIDs)},
		{"common", "speed", strconv.FormatFloat(request.Speed, 'f', -1, 64)},
		{"common", "jobs", strconv.Itoa(request.Jobs)},
		{"common", "work_enabled", strconv.FormatBool(request.WorkEnabled)},
		{"common", "work_prefetch_enabled", strconv.FormatBool(request.WorkPrefetchEnabled)},
		{"captcha", "enabled", strconv.FormatBool(request.CaptchaEnabled)},
		{"tiku", "provider", chain},
		{"tiku", "submit", strconv.FormatBool(request.Submit)},
		{"tiku", "cover_rate", strconv.FormatFloat(request.CoverRate, 'f', -1, 64)},
	}
	if providerChainHas(chain, "TikuYanxi") {
		updates = append(updates, iniUpdate{"tiku", "tokens", strings.TrimSpace(request.Tokens)})
	}
	if providerChainHas(chain, "TikuGo") {
		updates = append(updates, iniUpdate{"tiku", "go_authorization", strings.TrimSpace(request.GoAuthorization)})
	}
	if providerChainHas(chain, "AI") {
		updates = append(updates,
			iniUpdate{"tiku", "llm_preset", strings.TrimSpace(request.LlmPreset)},
			iniUpdate{"tiku", "endpoint", strings.TrimSpace(request.Endpoint)},
			iniUpdate{"tiku", "key", request.APIKey},
			iniUpdate{"tiku", "model", strings.TrimSpace(request.Model)},
		)
	}
	merged := mergeINI(content, updates)
	file, err := os.CreateTemp(filepath.Dir(absolute), ".config-*.ini")
	if err != nil {
		return "", fmt.Errorf("保存配置文件失败：%w", err)
	}
	defer os.Remove(file.Name())
	if err := file.Chmod(0o600); err != nil {
		file.Close()
		return "", err
	}
	if _, err := file.WriteString(merged); err != nil {
		file.Close()
		return "", fmt.Errorf("写入配置文件失败：%w", err)
	}
	if err := file.Close(); err != nil {
		return "", fmt.Errorf("关闭配置文件失败：%w", err)
	}
	if err := os.Rename(file.Name(), absolute); err != nil {
		return "", fmt.Errorf("替换配置文件失败：%w", err)
	}
	if err := rememberConfig(absolute); err != nil {
		return "", err
	}
	return absolute, nil
}

func knownTikuProvider(value string) bool {
	switch value {
	case "", "TikuYanxi", "TikuGo", "TikuLike", "TikuAdapter", "AI", "LLM", "SiliconFlow", "TikuManual":
		return true
	}
	return false
}

func knownProviderChain(chain string) bool {
	for _, provider := range splitProviderChain(chain) {
		if !knownTikuProvider(provider) {
			return false
		}
	}
	return true
}

func mergeINI(content string, updates []iniUpdate) string {
	lines := strings.Split(strings.TrimSuffix(content, "\n"), "\n")
	seen := make(map[string]bool)
	sections := make(map[string]bool)
	section := ""
	var result []string
	flush := func() {
		for _, update := range updates {
			id := update.section + "\x00" + update.key
			if update.section == section && !seen[id] {
				result = append(result, update.key+" = "+update.value)
				seen[id] = true
			}
		}
	}
	for _, line := range lines {
		trimmed := strings.TrimSpace(line)
		if strings.HasPrefix(trimmed, "[") && strings.HasSuffix(trimmed, "]") {
			flush()
			section = strings.TrimSpace(trimmed[1 : len(trimmed)-1])
			sections[section] = true
		}
		if section != "" && !strings.HasPrefix(trimmed, "#") && !strings.HasPrefix(trimmed, ";") {
			if key, _, ok := strings.Cut(trimmed, "="); ok {
				for _, update := range updates {
					if update.section == section && update.key == strings.TrimSpace(key) {
						line = update.key + " = " + update.value
						seen[section+"\x00"+update.key] = true
						break
					}
				}
			}
		}
		result = append(result, line)
	}
	flush()
	for _, update := range updates {
		if !sections[update.section] {
			result = append(result, "", "["+update.section+"]")
			sections[update.section] = true
		}
		id := update.section + "\x00" + update.key
		if !seen[id] {
			result = append(result, update.key+" = "+update.value)
			seen[id] = true
		}
	}
	return strings.Join(result, "\n") + "\n"
}
