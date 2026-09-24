import { ArrowDown, ArrowUp } from "lucide-react";
import { useI18n } from "../i18n";
import {
  MACHINE_OSES,
  machineOsLabel,
  normalizedPriority,
  SHELL_BACKENDS_BY_OS,
  shellBackendLabel
} from "../lib/machineShells";
import type { MachineOs, ShellBackend, ShellPriority } from "../types";
import { IconButton } from "./Common";
import { SettingsPageHeading } from "./SettingsPageHeading";

interface ShellPrioritySettingsProps {
  priority: ShellPriority | undefined;
  onChange: (priority: ShellPriority) => void;
}

/** Whether a list is the OS's default order, which is stored as no list at all. */
function isDefaultOrder(os: MachineOs, order: readonly ShellBackend[]): boolean {
  const defaults = SHELL_BACKENDS_BY_OS[os];
  return order.length === defaults.length && order.every((backend, index) => backend === defaults[index]);
}

/**
 * One shell priority list per operating system. Its only use: when a machine
 * is added, the first shell in its OS's list that the machine has becomes its
 * agent shell. Only the combinations Mework supports are listed, so each list
 * holds exactly its OS's shells.
 */
export function ShellPrioritySettings({ priority, onChange }: ShellPrioritySettingsProps) {
  const { t } = useI18n();
  const move = (os: MachineOs, from: number, to: number) => {
    const order = normalizedPriority(os, priority?.[os]);
    const [backend] = order.splice(from, 1);
    order.splice(to, 0, backend!);
    const next: ShellPriority = { ...(priority ?? {}) };
    if (isDefaultOrder(os, order)) delete next[os];
    else next[os] = order;
    onChange(next);
  };
  return (
    <section className="settings-page shell-priority">
      <SettingsPageHeading
        title={t("Shell 优先级", "Shell priority")}
        description={t(
          "新添加一台机器时，Mework 会探测它的系统和可用 shell，把该系统列表里排得最靠前、且这台机器上有的 shell 设为它的代理 shell——远程文件工具和语言服务器都经它运行。已有机器的选择不受影响，可在各自的机器设置里修改。",
          "When a machine is added, Mework probes its system and shells and makes the highest-ranked shell in that system's list that the machine has its agent shell — the shell its remote file tools and language servers run through. Machines that already have one keep it; change it in each machine's settings."
        )}
      />
      {MACHINE_OSES.map((os) => {
        const order = normalizedPriority(os, priority?.[os]);
        return (
          <section key={os} className="shell-priority__os" aria-label={machineOsLabel(os)}>
            <h4>{machineOsLabel(os)}</h4>
            <ol className="shell-priority__list">
              {order.map((backend, index) => (
                <li key={backend} className="shell-priority__item">
                  <span className="shell-priority__rank">{index + 1}</span>
                  <span className="shell-priority__name">{shellBackendLabel(backend)}</span>
                  <IconButton
                    label={t("上移 {shell}", "Move {shell} up", { shell: shellBackendLabel(backend) })}
                    disabled={index === 0}
                    onClick={() => move(os, index, index - 1)}
                  >
                    <ArrowUp size={13} />
                  </IconButton>
                  <IconButton
                    label={t("下移 {shell}", "Move {shell} down", { shell: shellBackendLabel(backend) })}
                    disabled={index === order.length - 1}
                    onClick={() => move(os, index, index + 1)}
                  >
                    <ArrowDown size={13} />
                  </IconButton>
                </li>
              ))}
            </ol>
          </section>
        );
      })}
    </section>
  );
}
