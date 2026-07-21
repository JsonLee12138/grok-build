# Spike：Provider 配置归一化与回退路由可行性

## 目标

验证 Provider/Model 新配置是否可以不改写底层 sampler，以及现有 retry 分类是否足以支撑按真实 `provider/model` 的回退链。

## 时间盒

30 分钟（代码盘点与最小测试验证）。

## 做了什么

- 检查 `ConfigModelOverride`、`resolve_model_list`、`resolve_credentials`、`sampling_config_for_model` 的数据流。
- 检查三类 backend 的 sampler 与 SSE stream 归一化入口。
- 检查 retry 分类、退避和 `max_retries` 的实现位置。
- 对 `xai-grok-shell` 的配置凭据解析测试启动编译验证；测试在本次时间盒内仍处于大型 workspace 编译阶段，未得到最终通过/失败结果。
- 对照 xAI、Anthropic、Ollama 官方协议资料，核对 endpoint、认证、模型发现和 Responses/Messages 的差异。

## 结果

### 已验证

- **可行**：Provider 层可以在配置解析阶段展开为现有 `ModelEntry`；底层 sampler 已接收 endpoint、认证 scheme、backend、headers、模型真实 ID 和重试参数。
- **可行**：Chat Completions、Responses、Messages 已有独立请求/响应路径，能复用上层模型调用流程，但不能共用同一 wire adapter。
- **部分可行**：现有 retry 分类能识别可重试 HTTP/传输错误，但它只在同一模型客户端内重试；跨模型切换需要新增路由 attempt 状态和错误后选择逻辑。
- **发现缺口（已由当前配置契约解决）**：旧实现同时支持 `api_key` 与 `env_key` 并存在优先级；当前目标设计改为单一 `api_key` 联合类型，明文与 `{ env = "ENV_NAME" }` 互斥，再分别适配到既有内部字段，不再新增外部优先级规则。
- **发现缺口**：当前代码未发现 alias 唯一索引、稳定 `provider/model` 解析或配置化模型回退链。

## 结论

**需要更多信息后可实现，但技术路线可行。** 建议下一步先定义真实 provider ID、alias 冲突策略、回退总预算和流式/工具调用的 fail-closed 边界；随后以归一化 `ModelEntry` 为基础实现配置解析和纯函数路由选择测试，再接入实际请求循环。

## 后续验证建议

- 用 mock server 验证 401/403/400/422 不切换，429/5xx/timeout 按顺序切换。
- 验证流式首字节、首个工具事件和工具执行后的切换边界。
- 验证环境变量同时存在、文件 key 存在时的优先级和日志脱敏。
- 验证 `/v1/models` 同步不覆盖人工模型，且同步失败保留上次有效快照。
