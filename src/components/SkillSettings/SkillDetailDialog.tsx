import { Clock, ExternalLink, Wrench } from "lucide-react";
import type { JSX } from "react";
import { useI18n } from "../../i18n";
import type { SkillRecord } from "../../types";
import { Dialog } from "../Common";

/** Read-only skill details; enablement and removal each have one card-row entry point. */

function formatDate(value: string): string {
  const date = new Date(value);
  return Number.isNaN(date.getTime()) ? value : date.toLocaleString();
}

export function SkillDetailDialog({
  skill,
  onClose
}: {
  skill: SkillRecord;
  onClose: () => void;
}): JSX.Element {
  const { t } = useI18n();
  const sourceLabel = skill.source === "local_directory"
    ? t("本地目录", "Local directory")
    : skill.source === "zip"
      ? t("ZIP 压缩包", "ZIP archive")
      : skill.source === "remote"
        ? t("在线注册表", "Online registry")
        : t("系统技能", "System skill");

  return (
    <Dialog title={t("技能详情", "Skill details")} width="620px" onClose={onClose}>
      <div className="skill-detail">
        <div className="skill-detail__head">
          <span className="skill-detail__icon"><Wrench size={22} strokeWidth={1.5} /></span>
          <div className="skill-detail__title">
            <strong>{skill.name}</strong>
            <span className="skill-detail__chips">
              <em>{t("技能", "Skill")}</em>
              <span>{sourceLabel}</span>
              {skill.author && <span>{skill.author}</span>}
              {skill.version && <span>{skill.version}</span>}
              {skill.tags.slice(0, 3).map((tag) => <span key={tag}>{tag}</span>)}
            </span>
          </div>
        </div>

        <div className="skill-detail__installed">
          <span aria-hidden="true" />
          {t("已安装", "Installed")}
          {skill.sourceUrl && (
            <a href={skill.sourceUrl} target="_blank" rel="noreferrer noopener">
              {t("查看源码", "View source")}
              <ExternalLink size={11} aria-hidden="true" />
            </a>
          )}
        </div>

        <section className="skill-detail__section">
          <h3>{t("描述", "Description")}</h3>
          <p>{skill.description || t("暂无描述", "No description")}</p>
        </section>

        <div className="skill-detail__divider" />

        <dl className="skill-detail__meta">
          <div>
            <dt>{t("创建时间", "Installed")}</dt>
            <dd><Clock size={12} aria-hidden="true" />{formatDate(skill.installedAt)}</dd>
          </div>
          <div>
            <dt>{t("最近更新", "Last updated")}</dt>
            <dd><Clock size={12} aria-hidden="true" />{formatDate(skill.updatedAt)}</dd>
          </div>
          <div>
            <dt>{t("目录名", "Folder")}</dt>
            <dd><code>{skill.folderName}</code></dd>
          </div>
          <div>
            <dt>{t("来源位置", "Source location")}</dt>
            <dd><code title={skill.sourceLocation}>{skill.sourceLocation || t("未记录", "Not recorded")}</code></dd>
          </div>
        </dl>
      </div>
    </Dialog>
  );
}
