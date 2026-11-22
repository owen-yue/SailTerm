//! SSH keyboard-interactive 认证（RFC 4252）：服务端逐轮下发提示，用户经前端作答。
//!
//! 与主机密钥裁决同一套 `request_id → oneshot` 请求-应答机制（独立成表，两码事不混用）。
//! 安全红线：用户答案属**入向**凭证，只经 IPC 进入后端握手路径（同连接表单密码），
//! 事件载荷只含服务端提示文案（banner 本身就是要展示给用户的），绝不回带答案。

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use russh::client::KeyboardInteractiveAuthResponse;
use serde::Serialize;
use tauri::Manager;
use tokio::sync::oneshot;

use crate::vault::Secret;

/// 待用户作答的认证提示请求事件。
pub const EVENT_KBD: &str = "ssh-kbd-request";
/// 等待用户作答上限；超时即认证失败（fail-safe，同主机密钥裁决）。
const DECISION_TIMEOUT: Duration = Duration::from_secs(120);
/// 交互轮次上限：防畸形/抽风服务端把提示循环发个没完。
const MAX_ROUNDS: usize = 8;

/// 服务端下发的一条提示（`Prompt` 非 serde，转成自有 DTO 再给前端）。
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct KbdPromptItem {
    /// 提示文本（如 `Password:`、`Answer:`、`OTP token:`）。
    pub prompt: String,
    /// 是否允许回显；`false` 时前端按密码框渲染。
    pub echo: bool,
}

/// 抛给前端的作答请求负载（不含任何已有答案）。
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct KbdPrompt {
    pub request_id: String,
    pub host: String,
    /// 服务端横幅（协议里的 Name 字段，可能为空）。
    pub banner: String,
    /// 整体指令说明（如 "Enter your OTP token"，可能为空）。
    pub instructions: String,
    /// 本轮待答提示（空数组 = 服务端只发 banner，前端提示直接提交）。
    pub prompts: Vec<KbdPromptItem>,
}

/// 待作答请求表：`request_id → 应答通道`。值为 `Option<Vec<Secret>>`：
/// `Some` = 用户提交的答案（可能为空答，缺位由 `normalize` 补齐）；`None` = 用户主动取消，
/// 与“提交了空字符串”必须分开——前者绝不向服务端提交，后者是正常应答。
#[derive(Default)]
pub struct KbdPending {
    map: Mutex<HashMap<String, oneshot::Sender<Option<Vec<Secret>>>>>,
}

impl KbdPending {
    /// 登记一轮请求；顺带回收等待端已放弃的条目（receiver 已 drop 即无人再等，
    /// `is_closed()` 此时为真；仍在等人作答的 sender 必须留下）。
    pub fn register(&self, id: &str) -> oneshot::Receiver<Option<Vec<Secret>>> {
        let (tx, rx) = oneshot::channel();
        let mut map = self.map.lock().unwrap();
        map.retain(|_, tx| !tx.is_closed());
        map.insert(id.to_string(), tx);
        rx
    }

    /// 前端应答（`None` = 取消）；`false` 表示请求已失效（超时/重复应答）。
    pub fn respond(&self, id: &str, answers: Option<Vec<Secret>>) -> bool {
        self.map
            .lock()
            .unwrap()
            .remove(id)
            .is_some_and(|tx| tx.send(answers).is_ok())
    }

    /// 是否有正在等用户作答的 keyboard-interactive 请求：认证看门狗（`ssh::mod::guard_auth`）
    /// 据此暂停超时累计 —— kbd / 无密码弹窗要等人输入（另有 120s 裁决超时），不能被「无进展」误杀。
    /// 只数仍存活的 sender（receiver 已 drop 即等待端已放弃，不计入）。
    pub fn has_pending(&self) -> bool {
        self.map.lock().unwrap().values().any(|tx| !tx.is_closed())
    }
}

/// 把 russh 的 InfoRequest 字段转成前端 DTO。
fn to_prompt(
    request_id: &str,
    host: &str,
    name: &str,
    instructions: &str,
    prompts: &[AskPrompt],
) -> KbdPrompt {
    KbdPrompt {
        request_id: request_id.to_string(),
        host: host.to_string(),
        banner: name.to_string(),
        instructions: instructions.to_string(),
        prompts: prompts
            .iter()
            .map(|p| KbdPromptItem {
                prompt: p.prompt.clone(),
                echo: p.echo,
            })
            .collect(),
    }
}

/// 把答案与提示数对齐：缺位补空 `Secret`，多余的丢弃（协议按提示顺序逐位提交）。
fn normalize(answers: Vec<Secret>, want: usize) -> Vec<Secret> {
    let mut out = answers;
    out.resize(want, Secret::new(String::new()));
    out
}

/// keyboard-interactive 完整认证流：start → 逐轮出题作答 → Success/Failure。
///
/// `handle` 泛型化以便单测注入哑实现（真实调用只有一种 `Handle` 类型）；
/// `ask` 可注入替身（默认走前端弹窗），单测绕开 AppHandle。
pub(crate) async fn authenticate<H>(
    app: &tauri::AppHandle,
    host: &str,
    handle: &mut H,
    spec: &crate::session::ConnectSpec,
) -> Result<(), crate::error::AppError>
where
    H: KbdAuth,
{
    authenticate_with(Some(app), host, handle, spec, None).await
}

type RoundAsk = Box<
    dyn Fn(
        &Ask,
    ) -> futures::future::BoxFuture<'static, Result<Option<Vec<Secret>>, crate::error::AppError>>
        + Send
        + Sync,
>;

/// `ask_round` 的入参包（seam 签名不能过长，clippy fn_params_excessive_bools 同族管控）。
pub(crate) struct Ask {
    pub app: Option<tauri::AppHandle>,
    pub host: String,
    pub name: String,
    pub instructions: String,
    pub prompts: Vec<AskPrompt>,
}

/// 单条提示的中转 DTO（`russh::client::Prompt` 不实现 Clone，seam 需要可克隆形态）。
#[derive(Clone)]
pub(crate) struct AskPrompt {
    pub prompt: String,
    pub echo: bool,
}

async fn authenticate_with<H>(
    app: Option<&tauri::AppHandle>,
    host: &str,
    handle: &mut H,
    spec: &crate::session::ConnectSpec,
    ask_override: Option<RoundAsk>,
) -> Result<(), crate::error::AppError>
where
    H: KbdAuth,
{
    use crate::error::AppError;
    let ask: RoundAsk = match ask_override {
        Some(f) => f,
        None => Box::new(|a| {
            // 无 UI 上下文（重连/自动化入口）不可弹窗作答：按可展示的认证失败短路，
            // 不 panic——握手跑在 async 协程里，panic 会炸掉整条连接路径。
            let Some(app) = a.app.clone() else {
                return Box::pin(async {
                    Err(crate::error::AppError::Auth(
                        "keyboard-interactive 需要 UI 作答上下文".into(),
                    ))
                });
            };
            let host = a.host.clone();
            let name = a.name.clone();
            let instructions = a.instructions.clone();
            let prompts = a.prompts.clone();
            Box::pin(async move { ask_round(&app, &host, &name, &instructions, &prompts).await })
        }),
    };
    let mut response = handle
        .kbd_start(&spec.username, "")
        .await
        .map_err(|e| AppError::Auth(format!("keyboard-interactive 认证失败：{e}")))?;
    for _ in 0..MAX_ROUNDS {
        match response {
            KeyboardInteractiveAuthResponse::Success => return Ok(()),
            KeyboardInteractiveAuthResponse::Failure { .. } => {
                return Err(AppError::Auth("认证被拒绝：请检查用户名与应答".into()))
            }
            KeyboardInteractiveAuthResponse::InfoRequest {
                name,
                instructions,
                prompts,
            } => {
                let answers = ask(&Ask {
                    app: app.cloned(),
                    host: host.to_string(),
                    name,
                    instructions,
                    prompts: prompts
                        .iter()
                        .map(|p| AskPrompt {
                            prompt: p.prompt.clone(),
                            echo: p.echo,
                        })
                        .collect(),
                })
                .await?;
                // `None` = 用户主动取消：短路，绝不向服务端提交任何东西（也不会 panic）
                let answers = match answers {
                    Some(a) => a,
                    None => {
                        return Err(AppError::Auth(
                            "已取消 keyboard-interactive 认证".into(),
                        ))
                    }
                };
                response = handle
                    .kbd_respond(answers)
                    .await
                    .map_err(|e| AppError::Auth(format!("keyboard-interactive 应答失败：{e}")))?;
            }
        }
    }
    Err(AppError::Auth(
        "keyboard-interactive 交互轮次超限，已中止".into(),
    ))
}

/// 一轮出题：登记→emit→等作答→返回对齐后的答案（`None` = 用户取消）。
/// banner-only（零提示）也要问一遍前端：让用户看到横幅并确认继续。
async fn ask_round(
    app: &tauri::AppHandle,
    host: &str,
    name: &str,
    instructions: &str,
    prompts: &[AskPrompt],
) -> Result<Option<Vec<Secret>>, crate::error::AppError> {
    let request_id = uuid::Uuid::new_v4().to_string();
    let want = prompts.len();
    let reply = app
        .state::<crate::state::AppState>()
        .ssh_kbd
        .register(&request_id);
    let _ = tauri::Emitter::emit(
        app,
        EVENT_KBD,
        to_prompt(&request_id, host, name, instructions, prompts),
    );
    match tokio::time::timeout(DECISION_TIMEOUT, reply).await {
        // 提交（含逐位空答案）：正常应答，缺位由 normalize 补齐
        Ok(Ok(Some(answers))) => Ok(Some(normalize(answers, want))),
        // 用户主动取消：回 Ok(None)，由调用方短路（**绝不向服务端提交任何东西**）
        Ok(Ok(None)) => Ok(None),
        // 通道被 drop（无人应答）/超时：报错短路（替用户回空答会推进协议，可能被计入失败次数）
        Ok(_) => Err(crate::error::AppError::Auth(
            "已取消 keyboard-interactive 认证".into(),
        )),
        Err(_) => Err(crate::error::AppError::Auth(
            "等待作答超时（120s），认证中止".into(),
        )),
    }
}

/// 当服务端只支持 `password` 且本地无密码时，复用 kbd 弹窗基础设施向用户要一个密码。
/// 返回的 `Secret` 仅用于紧接着的 `authenticate_password` 调用，不落盘、不入日志。
pub(crate) async fn prompt_password(
    app: &tauri::AppHandle,
    host: &str,
) -> Result<Secret, crate::error::AppError> {
    let answers = ask_round(
        app,
        host,
        "Password Authentication",
        "",
        &[AskPrompt {
            prompt: "密码:".into(),
            echo: false,
        }],
    )
    .await?;
    // 取消（None）或报错都在上层短路；这里只处理「提交了但密码为空」
    let pw = match answers {
        Some(answers) => answers.into_iter().next().unwrap_or_else(|| Secret::new(String::new())),
        None => return Err(crate::error::AppError::Auth("已取消，认证中止".into())),
    };
    if pw.is_empty() {
        return Err(crate::error::AppError::Auth(
            "用户未输入密码，认证中止".into(),
        ));
    }
    Ok(pw)
}

/// 对 russh `Handle` 的窄抽象：keyboard-interactive 两步调用，单测以哑实现替身。
pub(crate) trait KbdAuth {
    fn kbd_start(
        &mut self,
        user: &str,
        submethods: &str,
    ) -> impl std::future::Future<Output = Result<KeyboardInteractiveAuthResponse, russh::Error>> + Send;
    fn kbd_respond(
        &mut self,
        answers: Vec<Secret>,
    ) -> impl std::future::Future<Output = Result<KeyboardInteractiveAuthResponse, russh::Error>> + Send;
}

impl KbdAuth for russh::client::Handle<super::host_key::ClientHandler> {
    async fn kbd_start(
        &mut self,
        user: &str,
        _submethods: &str,
    ) -> Result<KeyboardInteractiveAuthResponse, russh::Error> {
        // submethods 传 None：向服务端要它支持的全部提示（空串在个别实现里会被当筛选条件）
        self.authenticate_keyboard_interactive_start(user.to_string(), None)
            .await
    }

    async fn kbd_respond(
        &mut self,
        answers: Vec<Secret>,
    ) -> Result<KeyboardInteractiveAuthResponse, russh::Error> {
        // 明文只在这一刻进 russh（协议要求裸字符串），待答生命周期内始终是不清零不进日志的 Secret
        let answers = answers.iter().map(|s| s.expose().to_owned()).collect();
        self.authenticate_keyboard_interactive_respond(answers)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use russh::client::Prompt;
    use russh::MethodSet;

    fn prompts(n: usize) -> Vec<AskPrompt> {
        (0..n)
            .map(|i| AskPrompt {
                prompt: format!("p{i}"),
                echo: i % 2 == 0,
            })
            .collect()
    }

    fn spec() -> crate::session::ConnectSpec {
        crate::session::ConnectSpec {
            kind: crate::session::SessionKind::Ssh,
            host: "h".into(),
            port: 22,
            username: "u".into(),
            auth: crate::session::AuthMaterial::KbdInteractive,
            cols: 80,
            rows: 24,
            startup_dir: None,
            startup_command: None,
            keepalive_secs: 0,
            agent_forward: false,
            x11_forward: false,
            ftp_active_mode: false,
            via: None,
            serial: None,
            connect_timeout_secs: 0,
            compression: false,
            auto_record: false,
            record_dir: None,
            record_file_pattern: None,
            record_overwrite: false,
            record_timestamp_prefix: false,
            record_encrypt: true,
        }
    }

    /// 哑实现：按脚本依次吐应答包，并记录提交给服务端的答案供断言。
    struct FakeHandle {
        script: std::collections::VecDeque<KeyboardInteractiveAuthResponse>,
        sent: Vec<Vec<Secret>>,
    }

    impl KbdAuth for FakeHandle {
        async fn kbd_start(
            &mut self,
            _user: &str,
            _submethods: &str,
        ) -> Result<KeyboardInteractiveAuthResponse, russh::Error> {
            Ok(self.script.pop_front().unwrap())
        }

        async fn kbd_respond(
            &mut self,
            answers: Vec<Secret>,
        ) -> Result<KeyboardInteractiveAuthResponse, russh::Error> {
            self.sent.push(answers);
            Ok(self.script.pop_front().unwrap())
        }
    }

    /// 把已提交的答案展平成可比对的裸串（仅测试用）。
    fn exposed(sent: &[Vec<Secret>]) -> Vec<Vec<String>> {
        sent.iter()
            .map(|round| round.iter().map(|s| s.expose().to_owned()).collect())
            .collect()
    }

    fn info(n: usize) -> KeyboardInteractiveAuthResponse {
        KeyboardInteractiveAuthResponse::InfoRequest {
            name: String::new(),
            instructions: String::new(),
            prompts: (0..n)
                .map(|i| Prompt {
                    prompt: format!("p{i}"),
                    echo: i % 2 == 0,
                })
                .collect(),
        }
    }

    fn failure() -> KeyboardInteractiveAuthResponse {
        KeyboardInteractiveAuthResponse::Failure {
            remaining_methods: MethodSet::empty(),
            partial_success: false,
        }
    }

    #[tokio::test]
    async fn multi_round_success() {
        let mut h = FakeHandle {
            script: [info(1), info(2), KeyboardInteractiveAuthResponse::Success]
                .into_iter()
                .collect(),
            sent: Vec::new(),
        };
        let ask: RoundAsk = Box::new(|a| {
            let answers = a
                .prompts
                .iter()
                .enumerate()
                .map(|(i, _)| Secret::new(format!("ans{i}")))
                .collect();
            Box::pin(async move { Ok(Some(answers)) })
        });
        authenticate_with(None, "h", &mut h, &spec(), Some(ask))
            .await
            .unwrap();
        assert_eq!(exposed(&h.sent), vec![vec!["ans0"], vec!["ans0", "ans1"]]);
    }

    #[tokio::test]
    async fn failure_short_circuits() {
        let mut h = FakeHandle {
            script: [failure()].into_iter().collect(),
            sent: Vec::new(),
        };
        let err = authenticate_with(None, "h", &mut h, &spec(), None)
            .await
            .unwrap_err();
        assert!(matches!(err, crate::error::AppError::Auth(_)));
        // 失败终局不出题、不提交
        assert!(h.sent.is_empty());
    }

    #[tokio::test]
    async fn cancel_does_not_respond_to_server() {
        let mut h = FakeHandle {
            script: [info(1), KeyboardInteractiveAuthResponse::Success]
                .into_iter()
                .collect(),
            sent: Vec::new(),
        };
        // 用户取消的真实通路：应答通道收到 None（非报错注入），
        // 必须短路成失败且绝不替用户提交空答推进协议
        let ask: RoundAsk = Box::new(|_| Box::pin(async { Ok(None) }));
        assert!(authenticate_with(None, "h", &mut h, &spec(), Some(ask))
            .await
            .is_err());
        assert!(h.sent.is_empty());
    }

    #[tokio::test]
    async fn cancel_reaches_waiter_as_none() {
        let p = KbdPending::default();
        let rx = p.register("r1");
        assert!(p.respond("r1", None));
        // 已在 tokio 运行时内，用 `.await` 而非 `blocking_recv()`（后者在 async 上下文会 panic）。
        assert!(rx.await.unwrap().is_none());
    }

    #[test]
    fn register_respond_roundtrip() {
        let p = KbdPending::default();
        let rx = p.register("r1");
        assert!(p.respond("r1", Some(vec![Secret::new("a")])));
        let got = rx.blocking_recv().unwrap().unwrap();
        assert_eq!(vec!["a"], exposed(&[got])[0]);
        // 重复应答：请求已出表，返回 false（前端静默忽略）
        assert!(!p.respond("r1", Some(vec![])));
        assert!(!p.respond("nope", None));
    }

    #[test]
    fn register_reclaims_abandoned_rounds() {
        let p = KbdPending::default();
        // 等待端放弃（模拟超时后 read_turn 被 drop）→ 下次登记顺带回收；
        // 关键断言：活着的那条必须仍可应答（只数长度会被反向谓词恰好骗过）
        let rx = p.register("dead");
        drop(rx);
        let rx_alive = p.register("alive");
        assert!(p.respond("alive", Some(vec![Secret::new("x")])));
        assert_eq!(vec!["x"], exposed(&[rx_alive.blocking_recv().unwrap().unwrap()])[0]);
        assert!(!p.respond("dead", None), "已回收的死条目不应答");
    }

    #[test]
    fn to_prompt_maps_fields() {
        let dto = to_prompt("id-1", "host-a", "banner", "do it", &prompts(2));
        assert_eq!(dto.request_id, "id-1");
        assert_eq!(dto.host, "host-a");
        assert_eq!(dto.banner, "banner");
        assert_eq!(dto.instructions, "do it");
        assert_eq!(dto.prompts.len(), 2);
        assert!(dto.prompts[0].echo && !dto.prompts[1].echo);
    }

    #[test]
    fn normalize_aligns_answers_to_prompts() {
        // 缺位补空 Secret
        let padded = normalize(vec![Secret::new("x")], 3);
        assert_eq!(exposed(&[padded])[0], vec!["x", "", ""]);
        // 多余丢弃
        let cut = normalize(vec![Secret::new("a"), Secret::new("b")], 1);
        assert_eq!(exposed(&[cut])[0], vec!["a"]);
        // 正好
        assert!(normalize(vec![], 0).is_empty());
    }
}
