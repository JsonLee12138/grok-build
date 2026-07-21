# Provider 与自定义模型配置：技术可行性调研

## 当前交付范围说明

本报告调研了模型列表同步和跨模型 failover 的整体可行性，但当前交付已收窄为：只新增 Provider、Model、alias 的配置与读取，并在现有 model 使用入口映射为现有 `ModelEntry`。同步、failover、retry、sampler、session 和其他调用业务逻辑均不在本次实现范围；相关调研结论仅供未来独立需求参考。

当前配置契约进一步明确为：新 Provider/Model 只暴露一个 `api_key` 字段，使用 string 表示明文、使用 `{ env = "ENV_NAME" }` 表示环境变量引用，不再同时暴露 `api_key` 与 `env_key`；model lookup 按 alias、catalog key、wire slug 的顺序解析，alias 之间全局不得重名。下文关于旧实现双字段优先级的内容仅是现状事实，不是目标设计。

## 调研范围

本报告覆盖需求 `provider-model-configuration`：Provider 连接配置、模型目录与自定义模型、稳定 alias、模型 ID、协议适配、配置迁移，以及按真实 `provider/model` 标识执行回退链。报告不覆盖用量统计、计费结算和模型能力自动识别。

## 结论

需求整体可行，但不适合一次性改造为全新的调用架构。建议采用“Provider/Model 新配置格式 → 现有 `ModelEntry`/`SamplerConfig` 归一化”的增量路线：第一阶段先完成配置解析、稳定 ID/alias、协议与凭据一致性校验；第二阶段接入模型列表同步；第三阶段在现有单模型重试之上增加跨模型 failover。

首期协议优先级建议为：`chat_completions` 覆盖 OpenAI-compatible 服务，`responses` 显式 opt-in，`messages` 作为独立 Anthropic 适配路径。不能把三种协议视为只替换 URL 的同构接口。

当前最大阻塞点不是协议，而是配置语义和兼容性：旧实现同时存在 `api_key`/`env_key`，新 Provider 配置需要用单一 `api_key` 联合类型无歧义地适配到两种内部形态；alias-first 也需要统一现有多个 key/slug lookup 入口。跨模型回退链仍只是未来研究方向。

## 现有实现盘点

### 事实

- `~/.grok/config.toml` 已支持 `[model.<id>]` 稀疏覆盖和 `[models]` 全局默认值；解析优先级为用户配置 > prefetched 目录 > hardcoded defaults。见 [config.rs](/Users/jsonlee/Projects/grok-build/crates/codegen/xai-grok-shell/src/agent/config.rs:3128)。
- `ConfigModelOverride` 已包含 `base_url`、`model`、`api_key`、`env_key`、`api_backend`、`extra_headers`、`context_window` 等字段；无效字段会被剔除并保留模型，同时产生 warning。见 [config_model_override_parse.rs](/Users/jsonlee/Projects/grok-build/crates/codegen/xai-grok-shell/src/agent/config_model_override_parse.rs:1)。
- 运行时链路为 `ModelEntry → resolve_credentials → sampling_config_for_model → SamplerConfig`，已有 `chat_completions`、`responses`、`messages` 三类 backend，以及三类 SSE 流解析器。见 [config.rs](/Users/jsonlee/Projects/grok-build/crates/codegen/xai-grok-shell/src/agent/config.rs:4306) 和 [sampler stream](/Users/jsonlee/Projects/grok-build/crates/codegen/xai-grok-sampler/src/stream)。
- 当前凭据解析实现为：模型自身凭据（`api_key` 或 `env_key`）> session token > 全局 `XAI_API_KEY`；`api_base_url` 只在全局 API key fallback 场景优先于 `base_url`。见 [config.rs](/Users/jsonlee/Projects/grok-build/crates/codegen/xai-grok-shell/src/agent/config.rs:4306)。
- 当前已有按状态码/传输错误分类的单模型 retry、退避、`max_retries` 和 `inference_idle_timeout_secs`；未发现按配置的模型链切换实现。见 [retry.rs](/Users/jsonlee/Projects/grok-build/crates/codegen/xai-grok-sampler/src/retry.rs:102) 和 [request_task.rs](/Users/jsonlee/Projects/grok-build/crates/codegen/xai-grok-sampler/src/actor/request_task.rs:81)。
- 项目文档已提供 OpenAI、Anthropic、Ollama、Together AI 和本地兼容服务的静态配置示例。见 [11-custom-models.md](/Users/jsonlee/Projects/grok-build/crates/codegen/xai-grok-pager/docs/user-guide/11-custom-models.md:1)。

### 推断

- 现有内部抽象足以承载第一阶段 Provider 配置，不需要立即重写 sampler 或会话状态；新增 provider 层可以在解析后展开为现有 `ModelEntry`。
- 需求中的 alias、`provider/model` 和回退链属于目录/路由层能力，不应塞进 `SamplerConfig.model` 的协议字段语义；协议请求仍应发送 provider 真实模型 ID。
- 当前“无效字段保留模型”的容错策略有利于托管配置迁移，但必须增加“最终生效配置”和 warning 门禁，否则启动成功可能被误认为配置生效。

### 假设

- Provider 配置可能由用户、部署环境或远程托管配置提供，且配置变更需要可审计、可回滚。
- 请求可能携带敏感内容；自定义 endpoint 和原样 headers 因此属于安全边界，而非普通显示配置。
- failover 只针对尚未产生可交付结果的请求；流式响应或工具调用已经产生副作用后，不能无条件切换模型重放。

## 方案比较

| 方案 | 可行性 | 优点 | 主要代价 | 建议 |
|---|---|---|---|---|
| 延续 `[model.<id>]`，补 alias/fallback | 高 | 兼容性最好，复用现有 resolver | Provider 字段重复，难表达多模型共享连接 | 作为兼容层和第一阶段 |
| 独立 `[provider.<id>]` + `[model.<id>] provider = ...` | 高 | Provider 级 URL、认证、backend、headers 可复用 | 需要继承优先级、迁移和冲突诊断 | 作为目标配置格式 |
| Provider + connection/profile 多层模型 | 中 | 适合多环境、多账号、多租户 | 引用解析和 UI/诊断复杂度高 | 暂不作为首期 |

建议优先级明确为：模型字段 > provider 字段 > 全局默认 > 远程目录元数据 > 内置默认；旧 `[model.<id>]` 继续直接归一化为模型字段。该优先级是设计推断，当前代码尚未实现 provider 层。

## 协议与外部资料

- **事实**：xAI 官方 Quickstart 当前以 `/v1/responses` 和 Bearer API key 为例，同时保留 Chat Completions 兼容入口；模型列表 API 提供 `/v1/models` 与更丰富的 `/v1/language-models`。来源：[xAI Quickstart](https://docs.x.ai/developers/quickstart)、[xAI Models API](https://docs.x.ai/developers/rest-api-reference/inference/models)。
- **事实**：Anthropic Messages 使用独立的 Messages wire contract、`x-api-key`/版本头、`tool_use`/`tool_result` 和独立 SSE 生命周期；不能直接套用 OpenAI 的 `max_completion_tokens` 或 tool role。来源：[Anthropic Messages API](https://platform.claude.com/docs/en/api/messages/create)、[Anthropic streaming](https://platform.claude.com/docs/en/build-with-claude/streaming)。
- **事实**：Ollama 官方文档说明其只兼容 OpenAI API 的一部分；Chat Completions 支持 streaming/tools，Responses 为较新能力且不支持 stateful `previous_response_id`/`conversation`。来源：[Ollama OpenAI compatibility](https://docs.ollama.com/api/openai-compatibility)。
- **推断**：远程 `/v1/models` 只能可靠提供目录基础字段，不能自动推导 backend、tool calling、reasoning 或 schema 能力；这些应由人工配置或后续验证提供。
- **假设**：Provider 的实际兼容性不会仅由 URL 或模型名称决定，因此每个 provider/model 需要最小健康探测或人工能力声明。

## 推荐增量方案

1. 保留旧 `[model.<id>]`，新增 provider 解析层，最终生成现有 `ModelEntry`；为每个模型生成稳定真实 ID `provider/model`，并建立 alias → 真实 ID 的唯一索引。
2. 把凭据来源、认证 scheme、endpoint、backend、headers 作为一个一致性单元校验。新 Provider/Model 使用单一 `api_key` 联合类型，明文与环境变量引用互斥；适配时分别映射到现有内部 `api_key` 或 `env_key`，不修改旧配置的运行时优先级。
3. 静态模型优先；模型列表同步作为可选目录输入，不覆盖人工模型。同步结果只补充模型目录基础信息，人工配置继续覆盖 backend、context window、capability flags。
4. 在现有 retry 分类之上增加 route attempt：仅对超时、429、502/503/504、可确认的传输失败切换到回退链下一项；401/403、400/422、schema/参数错误直接终止。每次请求记录真实模型 ID、attempt index 和错误分类。
5. 流式输出或工具调用开始后默认禁止跨模型重放；若业务必须支持，需先建立请求幂等键、工具执行状态和副作用边界。

## 风险清单

| 风险 | 影响 | 缓解手段 |
|---|---|---|
| 新旧凭据形态混用 | 高：新 Provider 配置若同时落入内部 `api_key` 与 `env_key`，现有优先级会选错凭据 | 外部只允许单一 `api_key` 联合类型；适配时两个内部字段必须互斥 |
| 明文 API key / 原样 headers | 高：提交、备份、诊断或请求日志泄露 | 默认推荐 `api_key = { env = "ENV_NAME" }`；禁止日志输出值；限制敏感 header；检查配置文件权限 |
| endpoint 可替换 | 高：数据外传、模型目录污染、错误 TLS/域名信任 | HTTPS 与 allowlist；控制面/推理面分离；保留上一个可用目录并支持回滚 |
| 模型 alias 漂移/退休 | 中高：行为、价格、延迟变化 | 生产配置锁定版本或记录实际模型；alias 变更做回归与灰度 |
| 部分解析成功 | 高：模型仍出现在列表但 endpoint/backend/context 未生效 | `grok inspect` 输出最终归一化配置；warning 作为发布门禁 |
| context_window 默认 200000 | 中高：自动压缩和成本预算失真 | 未显式设置时标记为推断值；以官方能力或人工声明校验 |
| headers 覆盖认证/租户头 | 高：越权、错路由或跨租户 | 建立允许列表，禁止覆盖 Authorization、Host 和安全租户头 |
| retry 放大成本/副作用 | 高：重复工具调用、请求风暴 | 仅对幂等且可重试错误操作；指数退避、总时限、并发/预算闸门 |
| failover 重放流式请求 | 高：重复工具执行或上下文不一致 | 首字节/首个工具事件后 fail-closed；工具执行要求幂等键和状态检查 |
| provider 能力误判 | 中高：请求字段不兼容、schema/tool 失败 | backend/capability 显式配置；按 provider/model 做健康探测和契约测试 |

## 开放问题

- 新 `api_key` 联合类型的环境变量引用在内部映射到既有 `env_key`，旧 `[model.*]` 双字段语义保持不变。
- 真实 ID 的 provider 部分采用配置 key、规范化 hostname，还是独立不可变 provider ID？
- alias 之间全局不得重名；重复 alias 的全部声明禁用并 warning，不采用 first-wins/last-wins。alias 与 catalog key/wire slug 重名时允许，并由 alias 优先。
- 模型列表同步失败时，是保留上次快照、保留静态模型，还是清空远程模型？建议保留静态模型和上次有效快照。
- 回退链的重试预算是每个模型独立，还是整条链共享？建议整条链共享总时限和请求预算。
- 流式输出何时算“已产生不可回放副作用”？需要由工具执行协议明确，而不是仅依赖 HTTP 状态。

## 最终判定

**可行，但需要分阶段实现。** 当前阶段复用现有 `ModelEntry`/sampler，只新增 Provider/Model 配置适配和 alias-first lookup；列表同步与跨模型 failover 另行设计。当前阶段必须保证单一 `api_key` 联合类型互斥映射到内部凭据字段，并统一所有 model lookup 入口。
