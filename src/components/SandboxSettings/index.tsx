import { RotateCcw, ShieldCheck } from "lucide-react";
import type { JSX } from "react";
import { useEffect, useState } from "react";
import { useI18n } from "../../i18n";
import { DEFAULT_SANDBOX_ALLOWLIST, defaultSandboxSettings, localSandboxSupport, setupLocalSandbox } from "../../lib/runtime";
import type { SandboxNetworkMode, SandboxSettings as SandboxSettingsType, SandboxSupport } from "../../types";
import { Switch } from "../Common";
import "./SandboxSettings.css";

interface SandboxSettingsProps {
  settings: SandboxSettingsType | undefined;
  onChange: (settings: SandboxSettingsType) => void;
}

/**
 * One rule per line. The textarea is parsed on every keystroke, so it cannot
 * render the parsed value back — a blank line being typed between two rules
 * would vanish — and holds what was typed until the field is left.
 */
function ListField({
  label,
  description,
  value,
  placeholder,
  onChange
}: {
  label: string;
  description: string;
  value: readonly string[];
  placeholder: string;
  onChange: (value: string[]) => void;
}): JSX.Element {
  const [draft, setDraft] = useState<string | null>(null);
  return (
    <div className="sandbox-settings-page__row sandbox-settings-page__row--vertical">
      <div className="sandbox-settings-page__copy">
        <strong>{label}</strong>
        <small>{description}</small>
      </div>
      <textarea
        className="input sandbox-settings-page__textarea"
        aria-label={label}
        spellCheck={false}
        placeholder={placeholder}
        value={draft ?? value.join("\n")}
        onChange={(event) => {
          setDraft(event.target.value);
          onChange(event.target.value.split("\n").map((line) => line.trim()).filter(Boolean));
        }}
        onBlur={() => setDraft(null)}
      />
    </div>
  );
}

/**
 * The sandbox one conversation's commands run in: one sandboxed agent process
 * per machine the conversation uses, enforced by the operating system
 * (Seatbelt on macOS, bubblewrap and seccomp on Linux and WSL 2, srt-win's
 * restricted account and firewall rules on Windows). A page of the
 * conversation-settings pane, so a preset opened in that pane carries one too:
 * the conversation is the smallest thing a sandbox is ever drawn around. Off
 * unless switched on; a machine that cannot sandbox refuses the command rather
 * than running it unsandboxed. Windows needs a one-time setup with
 * administrator rights, started here — that part belongs to the computer, not
 * to the conversation.
 */
export function SandboxSettings({ settings, onChange }: SandboxSettingsProps): JSX.Element {
  const { t } = useI18n();
  const current = settings ?? defaultSandboxSettings();
  const [support, setSupport] = useState<SandboxSupport | null | "loading">("loading");
  const [settingUp, setSettingUp] = useState(false);
  const [setupError, setSetupError] = useState<string | null>(null);
  useEffect(() => {
    let live = true;
    localSandboxSupport()
      .then((result) => { if (live) setSupport(result); })
      .catch((error: unknown) => {
        if (live) setSupport({ backend: "", available: false, detail: String(error), setup: false });
      });
    return () => { live = false; };
  }, []);
  const setUp = () => {
    setSettingUp(true);
    setSetupError(null);
    setupLocalSandbox()
      .then(setSupport)
      .catch((error: unknown) => setSetupError(String(error)))
      .finally(() => setSettingUp(false));
  };
  const update = (change: Partial<SandboxSettingsType>) => onChange({ ...current, ...change });
  const updateNetwork = (change: Partial<SandboxSettingsType["network"]>) =>
    onChange({ ...current, network: { ...current.network, ...change } });

  const backendName = (backend: string) => backend === "seatbelt"
    ? "Seatbelt"
    : backend === "bubblewrap"
      ? "bubblewrap"
      : backend === "srt-win"
        ? t("Windows 沙箱", "Windows sandbox")
        : backend;
  const status = support === "loading"
    ? t("正在检查这台电脑……", "Checking this computer…")
    : support === null
      ? t("浏览器预览中无法检查。", "Cannot be checked in the browser preview.")
      : support.available
        ? t("这台电脑可以使用：{backend}", "Available on this computer: {backend}", { backend: backendName(support.backend) })
        : t("这台电脑不可用：{detail}", "Not available on this computer: {detail}", {
          detail: support.detail || backendName(support.backend)
        });
  const unavailable = support !== "loading" && support !== null && !support.available;
  const needsSetup = unavailable && support.setup;

  return (
    <section className="sandbox-settings-page">
      <section className="settings-card">
        <div className="sandbox-settings-page__row">
          <div className="sandbox-settings-page__copy">
            <strong>{t("在沙箱中运行命令", "Run commands in the sandbox")}</strong>
            <small className={unavailable ? "sandbox-settings-page__status sandbox-settings-page__status--unavailable" : "sandbox-settings-page__status"}>{status}</small>
          </div>
          <Switch
            checked={current.enabled}
            label={t("在沙箱中运行命令", "Run commands in the sandbox")}
            onChange={(enabled) => update({ enabled })}
          />
        </div>
        {needsSetup && (
          <div className="sandbox-settings-page__row">
            <div className="sandbox-settings-page__copy">
              <strong>{t("设置 Windows 沙箱", "Set up the Windows sandbox")}</strong>
              <small>{t(
                "只需一次，需要管理员批准：创建一个隐藏的本地账户（srt-sandbox），沙箱里的命令以它的身份运行；再加几条防火墙规则，让它只能连 Mework 的代理。同一台电脑上的其他程序（如 Claude Code）已设置过的会直接沿用。",
                "Once, with an administrator's approval: creates a hidden local account (srt-sandbox) that sandboxed commands run as, and firewall rules that let it reach nothing but Mework's proxy. A setup another program on this computer made (Claude Code's, for one) is used as it is."
              )}</small>
              {setupError && (
                <small className="sandbox-settings-page__status sandbox-settings-page__status--unavailable" role="alert">
                  {setupError}
                </small>
              )}
            </div>
            <button type="button" className="button button--primary button--small" disabled={settingUp} onClick={setUp}>
              <ShieldCheck size={12} aria-hidden="true" /> {settingUp ? t("正在设置……", "Setting up…") : t("设置", "Set up")}
            </button>
          </div>
        )}
        <div className="sandbox-settings-page__row sandbox-settings-page__row--vertical">
          <div className="sandbox-settings-page__copy">
            <strong>{t("沙箱里的命令", "What sandboxed commands can do")}</strong>
            <ul className="sandbox-settings-page__list">
              <li>{t(
                "只能写对话在这台机器上的工作区、自己的临时目录和包缓存；工作区里会在沙箱外执行的文件（.git/hooks、.git/config、.mework、.vscode、.envrc 等）仍然只读。",
                "Write only the conversation's workspaces on the machine, its own temporary directory and package caches — and not the files in them that run outside the sandbox later (.git/hooks, .git/config, .mework, .vscode, .envrc and the like)."
              )}</li>
              <li>{t(
                "读不到凭据：SSH 与 GPG 密钥、云与包仓库令牌、钥匙串、浏览器资料、Mework 自己的密钥库和数据；名字像密钥的环境变量也会被去掉。",
                "Cannot read credentials: SSH and GPG keys, cloud and registry tokens, keychains, browser profiles, Mework's own vault and data. Environment variables named like secrets are removed."
              )}</li>
              <li>{t(
                "只能经沙箱外的代理联网，代理按下面的网络规则放行，且不连回环、内网或云元数据地址。",
                "Reach the network only through a proxy outside the sandbox, which applies the rules below and never connects to loopback, private or cloud-metadata addresses."
              )}</li>
              <li>{t(
                "看不到、也碰不到沙箱外的进程：Mework、代理进程和其他对话的进程。",
                "Cannot see or touch processes outside the sandbox: Mework, its agent, other conversations."
              )}</li>
            </ul>
            <small>{t(
              "适用于 shell 工具和后台命令，在本机、WSL 2 与 SSH 机器上由 Mework 的代理进程执行。Linux 与 WSL 需要系统自带的 bubblewrap；Windows 需要设置一次：这台电脑在这里设置，SSH 连接的 Windows 机器要在那台机器上以管理员身份设置（命令被拒绝时会给出要运行的命令）。机器不能沙箱时，命令会被拒绝，而不是在沙箱外运行。文件工具、预览服务器、LSP、MCP 服务器、hooks 和你自己打开的终端不在沙箱里。",
              "Applies to the shell tool and background commands, carried out by Mework's agent on this computer, in WSL 2 and on SSH machines. Linux and WSL need the system's bubblewrap; Windows needs setting up once — this computer here, a Windows machine over SSH by an administrator on that machine (a refused command names what to run). On a machine that cannot sandbox, commands are refused rather than run outside it. File tools, preview servers, language servers, MCP servers, hooks and terminals you open yourself are not sandboxed."
            )}</small>
          </div>
        </div>
      </section>
      <section className="settings-card">
        <div className="sandbox-settings-page__row">
          <div className="sandbox-settings-page__copy">
            <strong>{t("网络", "Network")}</strong>
            <small>{t(
              "按主机名放行：放行 github.com 就放行了经 github.com 能做的一切。",
              "Decided by host name: allowing github.com allows whatever can be done through github.com."
            )}</small>
          </div>
          <select
            className="input"
            aria-label={t("网络", "Network")}
            value={current.network.mode}
            onChange={(event) => updateNetwork({ mode: event.target.value as SandboxNetworkMode })}
          >
            <option value="allowlist">{t("只允许白名单", "Allowlist only")}</option>
            <option value="open">{t("任意公网主机", "Any public host")}</option>
            <option value="off">{t("不联网", "No network")}</option>
          </select>
        </div>
        {current.network.mode === "allowlist" && (
          <>
            <ListField
              label={t("白名单", "Allowlist")}
              description={t(
                "一行一个：example.com、*.example.com（只匹配子域），可加 :端口。回环或内网地址只有写明地址（如 localhost:5432）才可达。在 macOS 和 Windows 上，沙箱连本机的 localhost——包括它自己起的开发服务器——也要经代理，所以要写上 localhost 或 localhost:端口；Linux 与 WSL 上沙箱有自己的回环，里面的进程可以直接互连。",
                "One per line: example.com, *.example.com (subdomains only), optionally with :port. A loopback or private address is reachable only when named itself (localhost:5432). On macOS and Windows, the sandbox reaches this computer's localhost — its own dev servers included — through the proxy too, so list localhost or localhost:port; on Linux and WSL it has a loopback of its own, where its processes connect directly."
              )}
              value={current.network.allow}
              placeholder={"github.com\n*.npmjs.org\nlocalhost:5432"}
              onChange={(allow) => updateNetwork({ allow })}
            />
            <div className="sandbox-settings-page__actions">
              <button
                type="button"
                className="button button--ghost"
                onClick={() => updateNetwork({ allow: [...DEFAULT_SANDBOX_ALLOWLIST] })}
              >
                <RotateCcw size={12} aria-hidden="true" /> {t("恢复默认白名单", "Restore the default allowlist")}
              </button>
            </div>
          </>
        )}
        {current.network.mode !== "off" && (
          <ListField
            label={t("黑名单", "Blocklist")}
            description={t("任何模式下都不放行，先于白名单判断。", "Never allowed, in any mode; checked before the allowlist.")}
            value={current.network.deny}
            placeholder={"*.example.com"}
            onChange={(deny) => updateNetwork({ deny })}
          />
        )}
      </section>
      <section className="settings-card">
        <ListField
          label={t("额外可写目录", "Further writable directories")}
          description={t(
            "这个对话在各台机器上的沙箱都可写，在哪台机器上存在就在哪台生效。绝对路径或以 ~ 开头。",
            "Writable by this conversation's sandbox on whichever machine has them. Absolute, or starting with ~."
          )}
          value={current.writable}
          placeholder={"~/shared-data"}
          onChange={(writable) => update({ writable })}
        />
        <ListField
          label={t("额外禁读路径", "Further unreadable paths")}
          description={t(
            "内置的凭据位置之外，这个对话的沙箱也读不到这些。",
            "Besides the built-in credential locations, this conversation's sandbox cannot read these."
          )}
          value={current.denyRead}
          placeholder={"~/secrets"}
          onChange={(denyRead) => update({ denyRead })}
        />
      </section>
    </section>
  );
}
