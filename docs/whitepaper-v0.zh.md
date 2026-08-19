# agent-daemon v0 白皮书（二进制：agentd / agentdctl）

## Agent Native Domain 的端侧事件分发守护进程

**状态：Reference copy（英文版 `whitepaper-v0.md` 是 SoT）/ v0.1 Amended**
**目标读者：负责实现 `agent-daemon` 的 Coding Agent**
**日期：2026-08-19（v0.1 修订：2026-08-20）**

> v0.1 修订合并了设计评审决议（对应 ADR 见 `docs/adr/`），修订处以「v0.1:」标注。要点：in-flight 重投递去重（ADR-0001）、daemon 级配置文件、`agentdctl init` 负责建流、AckWait 默认值、agents.d 内容级唯一性、注销等待在途 Ack、DeliverPolicy 重放边界、Envelope 版本升级规则、stdin 并发写入。

---

## 摘要

`agentd` 是运行在一台具体机器上的轻量守护进程。它持续连接所在 Domain 的消息 Relay，从 NATS JetStream 接收发给本机 Agent 的事件，根据 `agent_id` 找到该 Agent 注册的本地 Handler，然后把事件作为标准输入交给 Handler 执行。

它不理解 Agent Loop，不管理 Context，不判断 Agent 是否存活，不负责拉起 Agent，不验证业务发送者，也不处理 Handler 的重试和失败恢复。所有这些与具体 Agent 有关的逻辑，都由 Agent 自己注册的 Handler 完成。

整体边界可以概括为：

```text
NATS JetStream
    负责让事件可靠存在，并在 agentd 离线时保留事件

agentd
    负责把一个事件转换成一次本地 executable invocation

Agent-owned Handler
    负责判断事件意味着什么，以及怎样找到、启动或通知真正的 Agent

Agent Runtime
    负责理解事件、构造 Context，并自主决定接下来的行动
```

`agentd` 的目标不是成为另一套 Agent Harness，而是成为 Agent Native Domain 中一块稳定、简单、可复用的端侧 Infrastructure。

---

# 1. 背景：为什么需要 `agentd`

## 1.1 从 Harness 内部走向 Agent Native Infrastructure

今天的大多数 Agent 仍然运行在某个 Harness 中。

Harness 通常负责：

* 维护模型 Session；
* 构造 Context；
* 驱动 Agent Loop；
* 注册和执行 Tool；
* 管理 Sandbox；
* 展示 Human Interface；
* 保存部分运行状态。

这种形态适合一次具体的 Agent 工作，却很容易把 Agent 的存在与某个运行中的 Harness 绑定起来。

当 Harness 进程结束时，Agent 往往也随之失去：

* 持续可达的消息入口；
* 对外稳定身份；
* 异步任务完成后的回调能力；
* 从休眠状态恢复工作的能力；
* 在不同 Runtime 或不同机器之间迁移的能力。

我们希望采用另一种思路：

> Agent 可以更换自己的 Harness，甚至启动一个修改后的继任 Agent，再由旧 Agent 退出。

在这种模型中，Agent 的一次 Generation 只是一个可以替换的软件个体。真正长期存在的，应当是 Harness 之外的世界：

* 消息；
* Repository；
* Artifact；
* 计算资源；
* 外部服务；
* Human；
* 其他 Agent；
* 它们之间的关系。

`agentd` 就位于这条边界上。

它让一台机器上的 Agent 即使当前没有运行，也可以继续被 Domain 中的外部 Infrastructure 联系。

---

## 1.2 `agentd` 在 Agent World 中的位置

Agent World 不是一个中央平台。

它由许多 Personal Domain、Enterprise Domain、公共服务和独立机器组成。每个 Domain 可以自己部署：

* IM；
* Email；
* Git；
* CI；
* Sandbox；
* NATS；
* Agent Host；
* 其他内部 Infrastructure。

`agentd` 不是全球服务，也不是跨 Domain 的统一 Runtime。

它是一个**每台机器本地运行的 daemon**：

```text
Personal Domain
├── Self-hosted NATS JetStream
├── Desktop A
│   └── agentd
│       ├── coding.main
│       └── assistant.personal
└── Server B
    └── agentd
        └── research.main
```

一个 `agentd` 可以承载多个 Agent。

v0 中，一个 `agent_id` 在同一时刻只由一个 `agentd` 负责消费。一个 Agent 可以迁移到另一台机器，但迁移过程由 Agent 或 Human 显式更新配置完成；v0 不实现自动选主和多机 Lease。

---

# 2. 核心定义

## 2.1 Domain

Domain 是一组共享信任、基础设施和管理边界的环境。

一个 Domain 可以属于：

* 一个用户；
* 一家公司；
* 一个团队；
* 一个家庭；
* 一个公共服务提供方。

v0 假设：

* Relay 与 `agentd` 属于同一个 Domain；
* 本机运行的 Agent 彼此默认可信；
* 不考虑多租户、零信任或复杂 Agent IAM。

---

## 2.2 Relay

Relay 是 Domain 内部长期在线的消息基础设施。

v0 使用 self-hosted NATS JetStream：

* 外部 Adapter 或 Worker 向 JetStream 发布事件；
* `agentd` 通过持久 Consumer 接收事件；
* `agentd` 离线时，事件继续保存在 JetStream 中；
* `agentd` 重连后继续消费。

Core NATS 只向当前在线的 Subscriber 投递消息，而 JetStream 增加了持久化、Consumer Progress 和重放，因此发送端与接收端不需要同时在线。

---

## 2.3 Agent ID

`agent_id` 是一个 Domain 内的逻辑 Agent 名称。

例如：

```text
coding.main
assistant.personal
research.market
```

它不是：

* PID；
* Container ID；
* Hostname；
* 某个 Pi Session ID；
* 某次 LLM Request ID。

它只表示：

> 这条事件应该交给哪个逻辑 Agent 注册的本地 Handler。

v0 约束：

```text
agent_id := segment ("/" segment)*
segment  := [a-z0-9][a-z0-9_-]{0,62}
```

`.` 不允许出现在 Segment 中，因此可以安全映射到 NATS Subject：

```text
coding.main
→ agent.events.coding.main
```

---

## 2.4 Handler

Handler 是 Agent 自己注册的本地 executable。

它可以是：

* 带 shebang 的 Python 脚本；
* Bash 脚本；
* Rust/Go 二进制；
* Node 程序；
* 任何能够从 stdin 读取 JSON Event 的可执行文件。

`agentd` 不限定 Handler 使用哪种语言。

Handler 负责所有 Agent-specific 逻辑，例如：

* 验证发送者身份；
* 检查消息签名；
* 判断消息是否可信；
* 判断 Agent 当前是否运行；
* 启动 Pi、Codex、DSH 或其他 Runtime；
* 决定 Queue、Steer、Ignore 或创建新 Session；
* 实现自己的重试；
* 实现自己的 File Lock；
* 将事件转发到其他机器；
* 处理 Agent 自我迁移；
* 将事件写入 Agent 自己的持久状态。

---

## 2.5 Agent Runtime

Agent Runtime 是真正执行 Agent Loop 的程序。

例如：

* Pi Coding Agent；
* Codex；
* DeepSeek Harness；
* 自定义 Python Agent；
* Agent 自己生成的下一代实现。

`agentd` 不直接认识任何一种 Runtime。

如果 Pi 需要特殊启动方式，这些逻辑写在 Pi Agent 的 Handler 中，而不是写进 `agentd` Core。

---

# 3. 设计原则

## 3.1 `agentd` 不是 Agent Harness

`agentd` 不得包含：

* LLM Client；
* Prompt Template；
* Context Builder；
* Agent Loop；
* Tool Registry；
* Memory；
* Planner；
* Subagent；
* Workflow Engine。

它只执行本地程序。

---

## 3.2 Policy 留在 Handler

`agentd` 不判断：

* 谁能给 Agent 发消息；
* 消息是否值得处理；
* Agent 是否应该被唤醒；
* 当前消息属于哪个 Session；
* 是否需要重试；
* 是否应该串行处理某个 Project；
* 是否应该申请新的 Container。

这些都是具体 Agent 的 Policy。

`agentd` 只提供 Mechanism。

---

## 3.3 本机 Agent 默认互相信任

v0 的目标是 Personal Domain 或单用户机器。

因此：

* 本机 Agent 可以动态更新 `agentd` 配置；
* 不实现 Agent-level Authentication；
* 不实现每个 Agent 的 Capability Token；
* 不实现本地多租户隔离；
* Handler 默认与 `agentd` 使用同一个 Unix 用户运行。

本机安全边界依赖普通文件权限和 Unix 用户边界。

未来如果发展为 Domain-level、多用户 `agentd`，再增加独立身份、权限和 Sandbox。

---

## 3.4 Relay 连接必须认证

虽然本机 Agent 默认可信，但 `agentd` 不能接受任意公网来源的事件。

`agentd` 与 NATS 之间必须使用经过认证的连接。

v0 推荐：

* 每个 `agentd` 一份独立 NATS `.creds`；
* Domain 使用一个 NATS Account；
* NATS over TLS；
* Credential 文件只允许运行 `agentd` 的用户读取。

NATS `.creds` 文件包含 User JWT 和用于签署服务器 Challenge 的 NKey Seed，应当像密码一样作为 Secret 管理。

这层认证只回答：

> 当前连接的确属于本 Domain 的合法 `agentd`。

它不回答 Event 中声明的业务发送者是否真实。

Sender Authentication 留给 Handler。

---

## 3.5 不追求严格 Exactly Once

JetStream 提供 at-least-once Delivery：未确认的消息可能被重新投递。

`agentd` 应当尽量屏蔽正常的网络重投，让下游在通常情况下只看到一次 Event。

但 v0 不引入：

* 两阶段提交；
* 分布式事务；
* Handler Recovery Probe；
* 本地 Durable Inbox；
* 完整 Exactly-once Protocol。

在极少数情况下，例如：

```text
Handler 已经产生副作用
→ agentd 尚未记录完成
→ 机器突然断电
```

同一个 Event 可能再次调用 Handler。

Handler 应当知道这一点，并在容易做到时使用 `event_id` 保持幂等。

---

# 4. 总体架构

```text
External Services
IM / Email / GitHub / CI / Custom API
                    │
                    ▼
          Adapter / Worker Layer
      验证外部协议、转换成 Event
                    │
                    ▼
       Self-hosted NATS JetStream
           Durable Event Relay
                    │
              Pull Consumer
                    │
                    ▼
                 agentd
        target agent_id → executable
                    │
                    ▼
          Agent-owned Handler
                    │
        check / wake / queue / steer
                    │
                    ▼
             Agent Runtime
```

需要注意：

* Adapter / Worker 不属于 `agentd`；
* NATS Server 不属于 `agentd`；
* Handler 不属于 `agentd` Core；
* Agent Runtime 不属于 `agentd`；
* Human Interface 不属于 `agentd`。

`agentd` 只负责 Relay 与本地 Handler 之间的最后一段分发。

---

# 5. NATS JetStream 设计

## 5.1 Stream

v0 使用一个共享 Stream：

```text
Stream Name:
    AGENT_EVENTS

Subjects:
    agent.events.>

Storage:
    File

Retention:
    LimitsPolicy
```

Limits Retention 允许消息在 Consumer Ack 后继续保留到 `MaxAge`、`MaxBytes` 或 `MaxMsgs` 达到限制，便于调试和人工 Replay；Ack 推进的是 Consumer Position，而不是直接删除 Stream 中的消息。

推荐默认值：

```text
MaxAge:
    7 days

Replicas:
    1 for personal/self-host v0

MaxBytes:
    configurable

MaxMsgSize:
    256 KiB for v0
```

v0.1: Stream 由 `agentdctl init` 显式创建或校准（使用 Operator 级 Credential，一次性操作）；`agentd` 运行时 Credential 只需 Consumer 相关权限，不需要建流权限。

较大的文件、图片或 Artifact 不应直接放入 Event Payload，应通过外部 Object Store、Git 或文件服务传递引用。

---

## 5.2 Subject

每个 Agent 对应一个 Subject：

```text
agent.events.<encoded-agent-id>
```

编码规则：

```text
coding.main
→ agent.events.coding.main

assistant.personal
→ agent.events.assistant.personal
```

`agentd` 不订阅一个全局 `agent.events.>` 然后自行过滤。

它为每个已注册 Agent 创建或绑定一个独立 Consumer。

---

## 5.3 Consumer

每个 `agent_id` 对应一个 Durable Pull Consumer。

例如：

```text
Agent ID:
    coding.main

Filter Subject:
    agent.events.coding.main

Durable Consumer:
    agent-<stable-hash-of-agent-id>
```

Consumer 配置：

```text
AckPolicy:
    Explicit

DeliverPolicy:
    All

MaxAckPending:
    max_concurrency
```

Pull Consumer 允许客户端自己控制什么时候拉取、一次拉取多少消息；Durable Consumer 会在客户端断开后保留消费进度。

v0 约束：

> 一个 Durable Consumer 同一时间只能由一个 `agentd` 负责。

如果两个 `agentd` 同时消费同一个 Agent 的 Consumer，JetStream 可能将消息分配给两个 Client。v0 将此视为配置错误，不实现 Lease、Ownership Election 或自动抢占。

v0.1: 若 Consumer 被服务端删除后重建，`DeliverPolicy: All` 会重放 Stream 中保留的全部历史（最多 `MaxAge`），这是已知语义；需要全新起点时可在 register 时使用 `--deliver-new`。正常路径不受影响——Durable Consumer 存在时总是从其保存的进度继续。

---

## 5.4 Ack 与长时间 Handler

`agentd` 在 Handler 进程结束后才最终 Ack。

如果 Handler 运行时间超过 Consumer 的 `AckWait`，JetStream 会认为 Consumer 失效并重新投递。JetStream 支持 `in-progress` Ack，用于重置 `AckWait` Timer，防止长时间任务被误判为失败。

v0.1 默认：`AckWait = 5m`，`in-progress` 间隔 `90s`，二者可配置。

因此 v0 SHOULD：

* 在 Handler 进程仍然存活时，周期性发送 `in-progress`；
* Handler 结束后发送最终 Ack；
* 不根据 Handler 的退出码选择 Ack、Nak 或 Retry。

`in-progress` 只是保持当前 Delivery Lease，不代表 `agentd` 负责 Handler 重试。

---

## 5.5 NATS Credential

每台机器上的 `agentd` 使用一份独立 Credential。

建议：

```text
Domain:
    one NATS Account

Machine:
    one NATS User / .creds file

Credential:
    stored locally with mode 0600
```

v0 可以给 `agentd` 较宽的 Domain 内权限：

* 读取 `agent.events.>`；
* 创建或更新对应 Consumer；
* 发布未来可能需要的状态消息。

NATS 权限本身是基于 Subject 的 Publish / Subscribe Allowlist；未来可以进一步限制每台机器允许消费的 Agent Subject。

v0 不要求实现这种细粒度限制。

---

# 6. Event Envelope v0

外部 Adapter 或 Worker 应把任意外部输入转换成一个最小 JSON Envelope。

示例：

```json
{
  "version": 1,
  "event_id": "01J6ZP8R5EF4Y42KABCD123456",
  "agent_id": "coding.main",
  "type": "im.message",
  "created_at": "2026-08-19T12:00:00Z",
  "payload": {
    "text": "请继续检查刚刚的测试结果"
  },
  "metadata": {
    "source": "matrix",
    "room_id": "!example:domain.test",
    "sender": "@alice:domain.test"
  }
}
```

## 6.1 必填字段

### `version`

Envelope 版本。

v0 只接受：

```json
"version": 1
```

v0.1: 未知 `version`（如 `2`）按 Terminal Event 处理——记录后 Ack，不重试，对发送端表现为事件被丢弃。版本规则：向后兼容的新增字段保持 `version: 1`；不兼容变更才提升版本号，且升级属于 Domain 内协调操作（先升级所有 `agentd`，再允许发送端产出新版本）。

### `event_id`

全局唯一、稳定的 Event Identifier。

推荐使用：

* UUIDv7；
* ULID；
* 其他可全局唯一生成的字符串。

`event_id` 用于尽量去重。

它不表示顺序。

### `agent_id`

目标 Agent。

必须与本地注册表中的 Agent ID 一致。

### `type`

事件类型提示。

例如：

```text
im.message
email.received
ci.completed
github.review
timer.fired
custom
```

`agentd` 不解释 `type`。

### `created_at`

外部事件生成时间，RFC 3339 UTC。

### `payload`

任意 JSON Value。

`agentd` 不解释 Payload。

---

## 6.2 可选字段

### `metadata`

任意 JSON Object。

可包含：

* Sender Claim；
* Signature；
* Reply Target；
* Conversation ID；
* Artifact Reference；
* Trace Context；
* External Service Metadata。

这些字段全部留给 Handler 解释。

---

## 6.3 `agentd` 允许解释的字段

`agentd` 只解释：

```text
version
event_id
agent_id
```

其他所有字段必须原样交给 Handler。

未知字段不得导致 Event 被拒绝。

这保证以后可以扩展 Envelope，而不需要同步升级所有 `agentd`。

---

# 7. 本地 Agent 注册

Daemon 自身的配置（v0.1）持久化在 `$XDG_CONFIG_HOME/agentd/agentd.toml`，包含：NATS URL、Credential 路径、Stream 名称、Dedup 存储路径与 TTL、Control Socket 路径、日志级别等。Agent 注册仍放在 `agents.d/`（见 7.4）。

## 7.1 注册模型

一台机器上的 `agentd` 可以注册多个 Agent：

```text
agentd
├── coding.main
├── assistant.personal
└── research.market
```

每个 Agent 注册：

* `agent_id`；
* Handler Path；
* 最大并发度；
* 可选 Working Directory；
* Enabled 状态。

示例配置：

```toml
agent_id = "coding.main"
handler = "/home/clouder/agents/coding-main/on-event"
max_concurrency = 1
working_directory = "/home/clouder/projects/main"
enabled = true
```

---

## 7.2 Handler Path

Handler Path 必须：

* 是绝对路径；
* 来自本地配置，而不是 Event；
* 指向本机可执行文件；
* 不经过 Shell 字符串拼接。

正确：

```text
execve("/home/clouder/agents/coding-main/on-event", ...)
```

错误：

```text
sh -c "<event supplied command>"
```

Python Handler 应带 shebang：

```python
#!/usr/bin/env python3
```

并设置 executable bit。

---

## 7.3 动态配置

Agent 必须能够动态注册、更新和注销自己。

v0 提供本地 Control Socket：

```text
$XDG_RUNTIME_DIR/agentd/control.sock
```

Socket 默认：

```text
mode 0600
```

不实现额外身份认证。同一 Unix 用户下的本地 Agent 默认可信。

最小操作：

```text
register
update
unregister
list
reload
```

推荐提供 CLI：

```bash
agentdctl register \
  --id coding.main \
  --handler /home/clouder/agents/coding-main/on-event \
  --max-concurrency 1 \
  --cwd /home/clouder/projects/main

agentdctl update coding.main \
  --handler /home/clouder/agents/coding-main-v2/on-event

agentdctl unregister coding.main

agentdctl list

agentdctl reload
```

---

## 7.4 持久配置

本地注册应持久化在：

```text
$XDG_CONFIG_HOME/agentd/agents.d/
```

每个 Agent 一份 TOML：

```text
agents.d/
├── coding-main.toml
├── assistant-personal.toml
└── research-market.toml
```

v0.1: `/`→`-` 的文件名映射不是单射（`a/b-c` 与 `a-b/c` 都得到 `a-b-c.toml`），因此唯一性以文件内容中的 `agent_id` 为准：加载时检测到重复 `agent_id` 即报错，文件名仅是显示约定。

更新必须采用：

```text
write temporary file
→ fsync if practical
→ atomic rename
```

`agentd` Reload 后：

* 新 Agent：创建或绑定 Consumer，并开始消费；
* 更新 Agent：未来 Event 使用新 Handler；
* 禁用 Agent：停止 Pull 新 Event；
* 删除 Agent：停止消费，但不自动删除 Stream 中原始消息；
* 已运行的 Handler 不因 Reload 被终止；
* v0.1：注销或禁用 Agent 时，需等待其所有在途 Handler 退出并完成 Dedup 写入与 Ack 之后，才释放 Consumer 绑定；期间不再 Pull 新消息。

---

# 8. Dispatch Contract

## 8.1 基本过程

对一条 JetStream Message，`agentd` 执行：

```text
1. Parse Event Envelope
2. Validate version, event_id, agent_id
3. Verify target matches current Agent registration
4. Wait for available concurrency slot
   （Pull 由空闲 slot 驱动；不持有超过当前可调度的消息）
5. Check dedup：completed store 与 in-flight 集合
   （v0.1：dedup 判定放在取得 slot 之后、Spawn 之前）
6. 若 event_id 已在 in-flight：丢弃本地副本且不 Ack，
   等待 JetStream 重投递后按 completed 去重
7. Spawn Handler，将 event_id 加入 in-flight 集合
8. 并发地向 Handler stdin 写入原始 Event JSON
   （v0.1：Event 可达 256 KiB，超过管道缓冲区，
   写入必须与等待退出并行执行）
9. Close stdin（Handler 提前退出造成的 EPIPE 属正常，不是错误）
10. Wait for Handler process to exit
11. Record event_id as completed，并移出 in-flight 集合
12. Ack JetStream Message（Double Ack）
13. Release concurrency slot
```

---

## 8.2 Handler 输入

Handler 通过 stdin 接收完整 UTF-8 JSON：

```bash
/path/to/on-event
```

stdin：

```json
{
  "version": 1,
  "event_id": "...",
  "agent_id": "coding.main",
  "type": "im.message",
  "created_at": "...",
  "payload": {},
  "metadata": {}
}
```

`agentd` 不修改 Payload。

---

## 8.3 Handler 环境变量

`agentd` MAY 提供：

```text
AGENTD_AGENT_ID
AGENTD_EVENT_ID
AGENTD_EVENT_TYPE
AGENTD_STREAM_SEQUENCE
AGENTD_CONSUMER_SEQUENCE
AGENTD_DELIVERY_COUNT
```

这些只是便利信息。

完整 Event 仍以 stdin 为准。

`agentd` 不得通过环境变量向 Handler 暴露 NATS Credential 内容。

---

## 8.4 Handler 输出

v0 不定义 Handler stdout Protocol。

Handler 可以自由输出日志。

stdout/stderr 可以由 `agentd` 继承或转发到自身日志系统。

---

## 8.5 Handler 退出码

这是 v0 最重要的语义之一：

> `agentd` 不根据 Handler 退出码进行重试。

无论 Handler：

```text
exit 0
exit 1
exit 127
被 signal 终止
```

本次 Dispatch 都视为已经结束。

`agentd` 应：

1. 记录退出状态；
2. 将 `event_id` 写入已完成去重记录；
3. Ack JetStream Message；
4. 不再次调用 Handler。

Handler 如果需要重试，必须在 Handler 内部实现。

例如：

```python
while True:
    try:
        deliver_to_agent(event)
        break
    except TemporaryError:
        time.sleep(1)
```

或者 Handler 可以把事件写入 Agent 自己管理的 Queue，然后立即退出。

`agentd` 不理解这些策略。

---

## 8.6 Handler Spawn Failure

如果 Handler：

* 文件不存在；
* 没有执行权限；
* 无法创建进程；
* Working Directory 不存在；

`agentd` 应：

1. 记录明确错误；
2. 将本次 Dispatch 标记为终结；
3. Ack 或 Term JetStream Message；
4. 不自动重试。

这种错误属于本地 Agent 注册或 Handler 实现问题，而不是 `agentd` 的重试问题。

---

## 8.7 Handler 应当是交接程序，而不是完整任务

推荐的 Handler 行为是：

```text
接收 Event
→ 判断怎样找到 Agent
→ 必要时启动 Agent
→ 把 Event 交给 Agent
→ 返回
```

真正持续数小时的工作应由 Agent Runtime、后台 Job 或其他 Service 执行。

Handler 可以为完成交接而短暂重试，但不应把整个长期 Agent Task 放在 Handler 进程中。

这不是硬性限制。`agentd` v0 不设置 Handler Timeout。

如果 Agent 选择让 Handler 长期运行，它需要接受：

* 占用一个 Concurrency Slot；
* 串行模式下阻塞后续 Event；
* Agent 自己负责其生命周期。

---

# 9. 并发与顺序

## 9.1 默认串行

每个 Agent 默认：

```toml
max_concurrency = 1
```

`agentd` 对该 Agent：

* 一次只 Pull 一条 Message；
* 等当前 Handler 退出并 Ack 后，再获取下一条；
* 因此维持 Consumer Delivery Order。

不需要额外 File Lock。

---

## 9.2 并发 Dispatch

Agent 可以注册：

```toml
max_concurrency = 4
```

此时：

* 最多同时运行四个 Handler；
* `agentd` 可以 Pull 最多四条未完成 Message；
* 完成顺序不保证；
* Handler 自己负责更细粒度的并发控制。

例如 Agent 想做到：

* 同一 Repository 串行；
* 不同 Repository 并行；
* 同一 Conversation 串行；

可以在 Handler 中使用：

* `flock`；
* SQLite；
* Python `filelock`；
* 自己的 Dispatcher；
* 自己的 Local Queue。

这些都不进入 `agentd`。

---

## 9.3 Stream Sequence

JetStream 为 Stream 和 Consumer 维护递增 Sequence。Consumer Metadata 还会提供 Delivery Count。

`agentd` 可以把这些值作为环境变量提供给 Handler，但不应把它们提升为 Agent World 的全局顺序语义。

在并发模式下，Handler 不得假定完成顺序与 Stream Sequence 一致。

---

# 10. 尽量一次的投递语义

## 10.1 目标

正常运行时，同一 `event_id` 应只调用一次 Handler。

网络重连、Ack 丢失或 JetStream 重投，不应在通常情况下直接导致 Handler 重复执行。

---

## 10.2 最小去重状态

`agentd` 不需要 Local Inbox。

它只维护一份很小的 Persistent Dispatch History，例如 SQLite：

```sql
CREATE TABLE completed_events (
    event_id TEXT PRIMARY KEY,
    completed_at INTEGER NOT NULL
);
```

收到 Event 时：

```text
event_id 已存在
→ 不 Invoke Handler
→ 直接 Ack Message
```

Handler 退出后：

```text
INSERT completed_events
→ Final Ack
```

记录可按 TTL 清理。

建议 Dedup Retention 大于 Stream `MaxAge`，或者至少覆盖常见 Ack 丢失与重连窗口。

## 10.3 在途重投递（v0.1）

Dedup Store 只记录已完成的 `event_id`。还存在一条不依赖崩溃的重复路径：Handler 运行期间机器 Suspend 或网络分区超过 `AckWait`，`in-progress` 无法到达服务端，恢复后 JetStream 会重投递仍在途的消息，而原 Handler 可能尚未退出。

处理方式（ADR-0001）：`agentd` 维护内存中的 in-flight `event_id` 集合；收到仍在 in-flight 的事件副本时，丢弃本地副本且不 Ack。服务端会在 `AckWait` 后再次投递，届时首个 Dispatch 已完成，completed 去重命中并 Ack。代价是该路径多一次 `AckWait` 级别的延迟；机器崩溃时 in-flight 集合随进程丢失，行为退化为 10.4 节的已知重复窗口。

---

## 10.4 可接受的重复窗口

以下情况仍然可能重复：

```text
Handler 已经产生副作用
→ Handler 尚未退出
→ agentd 或机器突然崩溃
→ completed_events 尚未写入
→ JetStream 重新投递
```

v0 接受该行为。

文档应明确：

> `agentd` 提供 best-effort effectively-once Dispatch，而不是严格 Exactly Once。

Handler 应在高风险场景中使用 `event_id` 实现幂等。

例如：

* 发邮件时使用外部 Service 的 Idempotency Key；
* 修改数据库时以 `event_id` 作为唯一键；
* 向 Agent Runtime 注入消息时去重；
* 部署操作先查询目标状态。

对于普通 IM 消息，极端情况下重复一次通常可以接受。

---

## 10.5 Ack 确认

Rust `async-nats` 支持普通 Ack 和等待服务器确认的 Double Ack；Double Ack 可以减少“本地认为已 Ack，但服务器未收到”产生的重投。

v0 SHOULD 在写入 `completed_events` 后使用 Double Ack。

如果 Double Ack 失败：

* 不删除本地 `completed_events`；
* Event 重投时由 Dedup Store 跳过 Handler；
* 再次 Ack 即可。

---

# 11. Handler 的完整责任

Handler 是 Agent 自己的 userspace Policy。

下面这些事情全部属于 Handler，而不是 `agentd`。

## 11.1 业务身份与授权

Handler 可以：

* 信任 Domain 内 IM Adapter 的 Sender Claim；
* 验证端到端签名；
* 检查 Allowlist；
* 检查 Capability Token；
* 对 Public Message 做 Spam Filter；
* 拒绝未授权请求。

`agentd` 不检查这些内容。

---

## 11.2 Agent 健康检查

Handler 可以：

```text
检查 Unix Socket
检查 PID
调用本地 Health API
检查 systemd service
检查 Container
```

`agentd` 不知道什么叫 Agent Online。

---

## 11.3 Agent 唤醒

Handler 可以：

```text
systemctl --user start pi-agent.service
启动本地 Python Agent
连接常驻 RPC
启动 Container
SSH 到其他机器
请求云端创建新 Runtime
```

`agentd` 只启动 Handler。

---

## 11.4 Queue / Steer / Interrupt

Handler 可以根据 Agent Runtime 能力决定：

* 将 Event 放入 Agent 自己的 Queue；
* 在下一次 Tool Boundary Steer；
* 创建新 Session；
* 启动新 Worker；
* 忽略；
* 通知 Human；
* 中断当前工作。

`agentd` 不理解这些语义。

---

## 11.5 Retry

如果 Handler 的交接过程可能遇到临时失败，Handler 自己重试。

`agentd` 不会因为：

* 非零退出码；
* Runtime 启动失败；
* Handler 抛异常；

再次调用 Handler。

如果 Handler 希望把复杂重试交给其他系统，它可以自行：

* 写入本地 Queue；
* 启动 Background Service；
* 提交 Temporal Workflow；
* 发布新的 Event；
* 保存失败状态并通知 Human。

---

## 11.6 自我进化与迁移

旧 Agent 可以：

1. 部署新的 Runtime；
2. 编写新的 Handler；
3. 测试新 Handler；
4. 调用 `agentdctl update` 更新注册；
5. 未来 Event 开始进入新 Handler；
6. 旧 Agent 退出。

`agentd` 不需要知道发生了一次 Agent Generation Handoff。

它只看到：

```text
同一个 agent_id
→ handler path changed
```

这正是 `agentd` 与 Agent 自我进化兼容的关键。

---

# 12. 本地信任与安全边界

## 12.1 v0 Threat Model

v0 面向：

* 单用户 Personal Domain；
* 同一 Unix 用户下的多个 Agent；
* 受信本地环境；
* Self-hosted NATS。

不防御：

* 本地 Agent 相互攻击；
* 恶意本地用户；
* Handler 读取其他 Agent 文件；
* Handler 消耗过多 CPU；
* Handler 修改 `agentd` 配置；
* 本地多租户隔离问题。

这些属于未来版本。

---

## 12.2 必须保留的基础安全

即使本地默认信任，`agentd` 仍必须：

* 只从已认证 NATS 连接接收 Message；
* 不把 Event 字段拼接进 Shell；
* 只执行本地注册表中的绝对 Handler Path；
* 对 Event Size 设置上限；
* 对 JSON 深度或解析资源设置合理限制；
* 将 NATS Credential 文件权限设为 0600；
* 不在日志中输出 Credential；
* 不把 NATS Credential 内容传给 Handler；
* v0.1：NATS Account 是事件注入边界——任何持有 Domain Credential 的主体都能向任意 Agent Subject 发布事件，过滤责任在 Handler；
* v0.1：需要向 NATS 发布消息的 Agent（A2A、回复）必须使用自己的 Credential，绝不复用 `agentd` 的 Credential。

---

## 12.3 运行权限

v0 推荐 `agentd` 作为 User Service 运行：

```text
systemd --user
```

Handler 使用与 `agentd` 相同的 Unix 用户。

不推荐以 root 运行。

如果未来需要：

* 切换 Unix 用户；
* 启动特权 Container；
* 操作系统级资源；

应增加独立的、窄接口 Privileged Helper，而不是直接扩大 `agentd` Core 权限。

---

# 13. Agent 配置更新与自管理

Agents 应能够更新本机 `agentd` 配置。

这是设计目标，而不是安全漏洞，因为 v0 默认本机 Agent 全信任。

典型场景：

```text
Agent 修改自己的 Runtime
→ 写出新的 Handler
→ 注册新 Handler
→ 验证
→ 更新 agentd Binding
→ 旧 Runtime 退出
```

Agent 也可以：

* 注册新的逻辑 Agent；
* 临时禁用自己；
* 改变并发度；
* 将 Handler 改成转发到另一台机器；
* 删除不再使用的 Agent。

`agentd` 不需要审批这些操作。

Human 仍可通过：

```bash
agentdctl list
agentdctl unregister
agentdctl update
```

查看和覆盖配置。

---

# 14. 运行与关闭

## 14.1 长期运行

`agentd` 预期大部分时间保持运行：

* 维持到 NATS 的连接；
* 管理多个 Durable Consumer；
* 等待 Event；
* 启动短期 Handler。

它应该足够轻量，以便在：

* Desktop；
* Laptop；
* 小型 VPS；
* 开发服务器；

上长期存在。

---

## 14.2 自身重启

`agentd` 可以重启。

可靠性来自：

* JetStream 保存未 Ack Event；
* Durable Consumer 保存消费位置；
* 本地 Completed Event Store 抑制常见重复；
* systemd 自动重启 daemon。

`agentd` 不需要通过“绝不死亡”获得可靠性。

---

## 14.3 Graceful Shutdown

收到 SIGTERM 时：

1. 停止 Pull 新 Message；
2. 等待当前 Handler 结束；
3. 对已结束 Handler 完成 Dedup Write 和 Ack；
4. 关闭 NATS；
5. 退出。

v0 可以允许 systemd 在 Shutdown Timeout 后强制结束。

如果在 Handler 运行中强制退出，Message 之后可能重投，Handler 可能重复执行。该行为属于已知语义。

---

# 15. 错误处理

## 15.1 NATS 断线

`agentd` 应持续尝试重连。

断线期间：

* 不 Pull 新 Message；
* JetStream 保留 Event；
* 本地正在运行的 Handler 不受影响；
* 重连后继续消费。

官方 Rust `async-nats` Client 提供 NATS、JetStream、TLS、Authentication 和 Reconnection 能力，适合作为 v0 实现。

---

## 15.2 无效 Event

以下情况视为 Terminal Event：

* JSON 无法解析；
* `version` 不支持；
* 缺少 `event_id`；
* 缺少 `agent_id`；
* `agent_id` 与 Consumer 不匹配；
* Event 超过大小限制。

`agentd` 应：

1. 记录错误；
2. Ack 或 Term；
3. 不重试。

否则一条 Poison Event 会永久阻塞 Consumer。

---

## 15.3 Handler 失败

Handler：

* 非零退出；
* 被 Signal 杀死；
* 内部异常；
* Agent 启动失败；

都不触发 `agentd` 重试。

`agentd` 只记录：

```text
agent_id
event_id
pid
exit_status
duration
```

然后完成 Dedup 与 Ack。

---

## 15.4 Handler 卡住

v0 不设置默认 Handler Timeout。

如果 Handler 长期不退出：

* 其 Concurrency Slot 一直被占用；
* `max_concurrency = 1` 时，后续消息继续留在 JetStream；
* `agentd` 持续发送 In-progress Ack；
* Human 或 Agent 可以手动终止 Handler。

未来如果实际需要，可以增加可选 Timeout，但不属于 v0 必须能力。

v0.1：不引入 Timeout，但 `agentd` 应在 Handler 运行超过阈值（默认 1h，可配置）时记录 Warning 日志，便于 Human 及早发现长期占用的 Slot。

---

# 16. 日志与可观察性

v0 使用结构化日志。

每条日志建议包含：

```text
timestamp
level
agent_id
event_id
consumer
stream_sequence
handler_path
handler_pid
duration_ms
exit_status
```

必须记录：

* NATS Connect / Disconnect；
* Agent Register / Update / Unregister；
* Consumer Create / Bind；
* Event Received；
* Dedup Hit；
* Handler Spawn；
* Handler Exit；
* Ack Success / Failure；
* Invalid Event；
* Spawn Failure。

建议支持：

```bash
agentdctl status
```

输出：

```text
NATS connection status
registered agents
consumer lag
active handler count
last event
last error
```

v0 不需要：

* Web UI；
* Prometheus Server；
* Distributed Trace；
* Agent Cognition Trace。

---

# 17. 推荐实现技术

## 17.1 语言

推荐 Rust Stable。

理由：

* 单二进制；
* 长期 daemon 的资源控制较好；
* 进程、Signal、Unix Socket 支持成熟；
* `async-nats` 是官方 NATS Rust Client；
* 适合以后扩展其他 Relay Adapter。

这不是架构硬性要求，但 Coding Agent 应优先采用 Rust。

---

## 17.2 推荐依赖类别

不冻结具体版本，但建议使用：

```text
tokio
async-nats
serde / serde_json
toml
clap
tracing
rusqlite or equivalent small embedded store
nix or tokio::process
```

不要引入：

* Web Framework；
* LLM SDK；
* Workflow Engine；
* Plugin Framework；
* Embedded Python；
* Container Runtime SDK。

---

## 17.3 代码模块

推荐结构：

```text
src/
├── main.rs
├── config.rs
├── registry.rs
├── control.rs
├── relay/
│   ├── mod.rs
│   └── nats.rs
├── event.rs
├── consumer.rs
├── dispatcher.rs
├── dedup.rs
├── process.rs
├── logging.rs
└── error.rs
```

其中：

### `relay/nats.rs`

负责：

* NATS Credential；
* JetStream Context；
* Stream / Consumer；
* Pull；
* Ack；
* In-progress；
* Reconnect。

### `registry.rs`

负责：

* AgentConfig；
* 注册表；
* Reload；
* Config Persistence。

### `dispatcher.rs`

负责：

* Concurrency Slot；
* Spawn Handler；
* stdin；
* Wait Exit；
* Dedup；
* Ack。

### `dedup.rs`

只负责近期已完成 `event_id`。

它不是 Inbox。

---

# 18. 本地控制协议

Control Socket 可以采用每行一个 JSON Request / Response。

示例：

```json
{
  "op": "register",
  "agent": {
    "agent_id": "coding.main",
    "handler": "/home/clouder/agents/coding-main/on-event",
    "max_concurrency": 1,
    "working_directory": "/home/clouder/projects/main",
    "enabled": true
  }
}
```

响应：

```json
{
  "ok": true
}
```

其他请求：

```json
{"op": "unregister", "agent_id": "coding.main"}
```

```json
{"op": "list"}
```

```json
{"op": "reload"}
```

协议不需要版本协商之外的复杂机制。

Socket 只接受本机连接。

---

# 19. Handler 示例

下面是一个概念性 Python Handler。

它不是 `agentd` 的一部分。

```python
#!/usr/bin/env python3

import json
import subprocess
import sys
import time


def runtime_is_ready() -> bool:
    result = subprocess.run(
        [
            "systemctl",
            "--user",
            "is-active",
            "--quiet",
            "pi-agent@main.service",
        ],
        check=False,
    )
    return result.returncode == 0


def ensure_runtime() -> None:
    if runtime_is_ready():
        return

    subprocess.run(
        [
            "systemctl",
            "--user",
            "start",
            "pi-agent@main.service",
        ],
        check=True,
    )

    while not runtime_is_ready():
        time.sleep(0.5)


def deliver(event: dict) -> None:
    subprocess.run(
        [
            "/home/clouder/bin/pi-deliver",
            "--event-id",
            event["event_id"],
        ],
        input=json.dumps(event).encode("utf-8"),
        check=True,
    )


def main() -> None:
    event = json.load(sys.stdin)

    # Sender auth, policy, dedup, retry and runtime integration
    # are all local Agent policy.
    ensure_runtime()
    deliver(event)


if __name__ == "__main__":
    main()
```

如果 `ensure_runtime()` 或 `deliver()` 可能临时失败，并且 Agent 希望重试，应在这段脚本内部实现。

`agentd` 不根据 Python 退出码重发 Event。

---

# 20. 端到端示例

## 20.1 注册 Agent

```bash
agentdctl register \
  --id coding.main \
  --handler /home/clouder/agents/coding-main/on-event \
  --max-concurrency 1 \
  --cwd /home/clouder/projects/main
```

`agentd`：

1. 保存配置；
2. 创建或绑定 Durable Consumer；
3. 开始 Pull `agent.events.coding.main`。

---

## 20.2 发布消息

IM Adapter 向：

```text
agent.events.coding.main
```

发布：

```json
{
  "version": 1,
  "event_id": "01J6ZP8R5EF4Y42KABCD123456",
  "agent_id": "coding.main",
  "type": "im.message",
  "created_at": "2026-08-19T12:00:00Z",
  "payload": {
    "text": "测试结果出来了吗？"
  },
  "metadata": {
    "source": "matrix",
    "sender": "@alice:example.com",
    "room_id": "!abc:example.com"
  }
}
```

---

## 20.3 Dispatch

`agentd`：

1. Pull Message；
2. Parse Envelope；
3. Dedup；
4. Spawn Handler；
5. 将 JSON 写入 stdin；
6. 等 Handler 退出；
7. 写入 `completed_events`；
8. Double Ack；
9. Pull 下一条 Message。

---

## 20.4 Handler

Handler：

1. 判断 Sender；
2. 检查 Pi Runtime；
3. 如有必要启动 Pi；
4. 将 Event 交给 Pi；
5. 内部处理自己的临时失败；
6. 退出。

之后 Pi 如何回复 Human，与 `agentd` 无关。

回复可以通过：

* Pi 自己调用 IM API；
* 另一个 Outbound Worker；
* 发布回 NATS；
* 任意 Agent 自己选择的方式。

v0 不统一 Agent 输出路径。

---

# 21. 测试要求

## 21.1 单元测试

必须覆盖：

* Agent ID 解析与 Subject 编码；
* Event Envelope 解析；
* 未知字段兼容；
* 配置校验；
* Registry Update；
* Dedup Store；
* Concurrency Gate；
* Handler Path Validation。

---

## 21.2 NATS 集成测试

必须使用真实 `nats-server` + JetStream 测试：

### 离线投递

1. 停止 `agentd`；
2. 发布 Event；
3. 启动 `agentd`；
4. Handler 被调用一次。

### 重连

1. `agentd` 在线；
2. 重启 NATS；
3. `agentd` 自动重连；
4. 后续 Event 正常投递。

### 多 Agent

1. 注册 Agent A 与 B；
2. 分别发布 Event；
3. 正确调用各自 Handler。

### 串行

1. `max_concurrency = 1`；
2. Handler A Sleep；
3. 发布多条 Event；
4. Handler 不重叠运行。

### 并发

1. `max_concurrency = 4`；
2. 发布多条 Event；
3. 同时最多四个 Handler。

### 非零退出不重试

1. Handler `exit 1`；
2. `agentd` 记录失败；
3. Message 被 Ack；
4. Handler 不再次调用。

### Spawn Failure 不重试

1. Handler Path 被删除；
2. 发布 Event；
3. `agentd` 记录 Spawn Error；
4. Message 被终结；
5. 不重复投递。

### Ack 丢失去重

1. Handler 完成；
2. Dedup Record 写入；
3. 模拟 Final Ack 失败；
4. JetStream 重投；
5. Handler 不再次执行；
6. `agentd` 重新 Ack。

### 在途重投递（v0.1）

1. Handler 运行中；
2. 冻结 `agentd` 进程或丢弃 `in-progress` 超过 `AckWait`；
3. JetStream 重投递；
4. `agentd` 不并发启动第二个 Handler；
5. 原 Handler 完成后，重投副本被 completed 去重命中并 Ack。

### Crash Window

1. Handler 已开始；
2. 强制终止 `agentd`；
3. 重启；
4. Event 可能重复；
5. 测试确认该行为与文档一致。

---

## 21.3 动态配置测试

必须覆盖：

* Runtime Register；
* Update Handler；
* Unregister；
* Disable / Enable；
* Reload；
* 更新并发度；
* Reload 时已有 Handler 不被中断；
* 新 Event 使用新 Handler。

---

# 22. 验收标准

v0 完成必须满足：

1. 一台机器上运行一个长期 `agentd`；
2. 使用 NATS Credential 连接 self-hosted JetStream；
3. 可动态注册多个 Agent；
4. 每个 Agent 有独立 Durable Pull Consumer；
5. Event 根据 `agent_id` 调用正确 Handler；
6. Handler 通过 stdin 收到原始 JSON；
7. 默认每 Agent 串行，可配置并发；
8. Handler 非零退出不触发重试；
9. Spawn Failure 不触发自动重试；
10. Agentd 不包含 Runtime Health、Wake、Queue、Steer 等逻辑；
11. Agentd 不验证业务 Sender；
12. Agentd 不维护 Local Inbox；
13. Agentd 维护最小 Completed Event Dedup；
14. NATS 重连后继续消费；
15. Agent 可以动态更新自己的 Handler；
16. 一个 Agent 可以切换到新 Handler 而无需重启整个 Domain；
17. 所有行为具有清晰结构化日志。

---

# 23. 明确非目标

v0 不实现：

* Agent Harness；
* LLM 调用；
* Context 管理；
* Agent Memory；
* Runtime Adapter Framework；
* 内建 Pi/Codex/DSH 支持；
* Sender Authentication；
* Agent-level IAM；
* Capability Token；
* Local Inbox；
* Local Outbox；
* Handler Retry；
* Handler Timeout；
* Strict Exactly Once；
* Dead Letter Queue；
* 多机选主；
* 一个 Agent 多个 Active `agentd`；
* 自动 Agent 迁移；
* 跨 Domain Federation；
* Agent Directory；
* A2A；
* MCP；
* IM Adapter；
* Web UI；
* Multi-user Sandbox；
* Domain-level Policy Engine。

如果实现过程中发现需要这些能力，不应直接加入 Core。应先在 Handler、外部 Adapter 或独立 Service 中实现，等待真实重复模式出现后再讨论是否抽象。

---

# 24. 未来可能的扩展

以下方向可以保留设计空间，但不应阻塞 v0：

* 多种 Relay Adapter；
* MQTT / Kafka / HTTP Relay；
* 多机 Agent Binding；
* Lease 与迁移；
* Domain-level、多用户 `agentd`；
* Handler Sandbox；
* WASM Handler；
* Agent Presence；
* Outbound Event Channel；
* Runtime Status Projection；
* Agent Directory；
* Cross-domain Contact Surface；
* 更强的 Local Isolation；
* 可选 Handler Timeout；
* 可选 Retry Policy。

`agentd` Core 应尽量保持：

```text
event
→ target lookup
→ executable invocation
```

---

# 25. 最终边界

整个设计可以压缩成四句话：

> **JetStream 负责让事件在 Agent 和机器离线时仍然存在。**

> **`agentd` 负责把事件转换成一次本地 executable invocation。**

> **Handler 负责所有与具体 Agent 有关的认证、唤醒、交接、重试和并发策略。**

> **Agent Runtime 负责理解事件，并自主决定接下来要做什么。**

`agentd` 的价值不来自它知道很多 Agent 概念，而恰恰来自它几乎不知道。

它不试图成为 Agent 的大脑、操作系统或控制中心。它只是一台机器与 Agent Native Domain 之间稳定、轻量的最后一段接线：

```text
maybe multiple agents
        ↑
      agentd
        ↕
NATS JetStream
```

这条边界足够窄，因此不会限制 Agent 未来修改自己的 Loop、替换自己的 Harness、迁移到新机器，或者产生一个新的继任者。

同时它又足够实用：Human、IM、CI 和其他 Infrastructure 发来的事件，可以在 Agent 当前没有运行时继续等待，并在机器重新连接后，交给 Agent 自己留下的本地程序。

这就是 `agentd` v0 应当实现的全部内容。
