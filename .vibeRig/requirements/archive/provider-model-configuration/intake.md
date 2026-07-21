# Provider 与自定义模型配置

## 当前交付范围

本次实现仅新增 Provider、Model、alias 的配置定义与读取，并在现有 model 使用入口将 alias 或 `provider/model` 映射为现有 `ModelEntry`。除该配置适配与 model lookup 接线外，不修改凭据解析、`SamplerConfig`、协议请求、stream、retry、session/current model、工具调用或其他业务逻辑。

下文涉及的 Provider 模型列表同步、自动路由和故障切换仅作为后续方向保留，不属于本次交付；后续需要实现时应单独立项和设计，不得从本需求拆入本期里程碑。

## 目标用户

面向项目中配置和使用模型的开发者及高级用户，使其能够接入内置列表之外的模型与 Provider，并通过稳定的模型标识完成业务调用。

## 痛点

当前系统只支持固定模型值，缺少独立的 Provider 配置，也不能录入任意模型。接入新供应商、新模型或同一供应商下的多个模型时，需要修改代码并等待版本发布；模型名称也缺少适合业务使用的稳定别名。

## 期望结果

- 用户可以新增、编辑和删除 Provider 配置。Provider 必须支持 API Base URL、API Key 和协议类型等必要连接信息。
- `api_key` 是唯一的凭据字段：可以直接写入明文，也可以使用 `{ env = "ENV_NAME" }` 引用环境变量；不再并列配置 `api_key` 与 `env_key`。
- Provider 的协议配置映射到项目已有的 OpenAI-compatible、Anthropic 和内置协议类型，不新增或修改协议实现。
- 用户可以手动配置自定义模型。
- 显式引用 Provider 的模型以 `provider/model` 作为 canonical key；不引用 Provider 的直接 Model 继续以 `[model.<id>]` 的表 key 作为 catalog key。
- 模型可以设置可选 alias。alias 之间全局不得重名；alias 可以与 model key 或 wire model slug 同名，model lookup 发生重名时 alias 优先。业务可以通过 alias 调用，未设置 alias 的模型可以直接通过真实 ID 调用。
- Provider Model 与直接 Model 都只接受统一的 `api_key` 联合语法，并在现有 model lookup 入口解析为现有 `ModelEntry`；独立 `env_key` 字段不再接受。

## 边界与非目标

- 本需求只聚焦 Provider、Model、alias 的配置定义、读取，以及在现有 model lookup 入口映射为既有 `ModelEntry`。
- 不包含 Provider 模型列表同步、自动路由、故障切换或回退链。
- 不包含对凭据解析、`SamplerConfig`、协议请求、stream、retry、session/current model、工具调用和其他模型调用业务逻辑的修改。
- 不包含模型用量统计、计费管理或成本结算。
- 不包含模型能力的自动识别与分类；本期只读取人工配置的模型信息。
- 不绑定产品级 PRD。

## 约束

- 实现必须适配当前 Rust 项目及既有模型调用协议。
- Provider 连接配置是必需能力，不能只将 Provider 作为模型 ID 的命名空间。
- 配置文件中的 API Key 属于敏感信息，不应出现在日志或普通界面的明文展示中。
- Provider、Provider Model 与不引用 Provider 的直接 Model 均使用统一的 `api_key` 联合语法；出现独立 `env_key` 时跳过对应 Model 并输出不含敏感值的 warning。
- 方案调研可参考 [NousResearch/hermes-agent](https://github.com/NousResearch/hermes-agent) 的 Provider 与模型配置方式，以及 [Anthropic](https://github.com/anthropics) 的协议实现资料，但最终实现需符合当前 Rust 项目的边界。

## 成功信号

- 接入新 Provider 或自定义模型不再要求修改固定模型列表的代码。
- 业务可以稳定使用 alias 或 `provider/model` 发起模型调用。
- alias 与 `provider/model` 均在现有 model lookup 入口解析为既有 `ModelEntry`。
- 不引用 Provider 的直接 Model 可继续配置自有 `model`、`base_url` 和 `api_key`，并通过表 key 或 wire model slug 调用。
