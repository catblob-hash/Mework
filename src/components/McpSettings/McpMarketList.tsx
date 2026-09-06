import { ExternalLink } from "lucide-react";
import type { JSX } from "react";
import { useI18n } from "../../i18n";
import { ProviderAvatar } from "../ProviderSettings/ProviderAvatar";

/**
 * Market page links to external MCP server directories.
 *
 * Production CSP permits only `img-src 'self' data:`, and no vendor logo assets
 * are bundled, so use the provider page's initial-letter avatar.
 */

interface Market {
  name: string;
  url: string;
  description: () => string;
}

export function McpMarketList(): JSX.Element {
  const { t } = useI18n();
  const markets: Market[] = [
    { name: "MCP World", url: "https://www.mcpworld.com", description: () => t("百度旗下 MCP 聚合平台", "Baidu's MCP aggregation platform") },
    { name: "BigModel MCP Market", url: "https://bigmodel.cn/marketplace/index/mcp", description: () => t("精选 MCP，极速接入", "Curated MCP servers with quick setup") },
    { name: "modelscope.cn", url: "https://www.modelscope.cn/mcp", description: () => t("魔搭社区 MCP 服务器", "ModelScope community MCP servers") },
    { name: "mcp.higress.ai", url: "https://mcp.higress.ai/", description: () => t("Higress MCP 服务器", "Higress MCP servers") },
    { name: "mcp.so", url: "https://mcp.so/", description: () => t("MCP 服务器发现平台", "MCP server discovery platform") },
    { name: "smithery.ai", url: "https://smithery.ai/", description: () => t("Smithery MCP 工具", "Smithery MCP tooling") },
    { name: "glama.ai", url: "https://glama.ai/mcp/servers", description: () => t("Glama MCP 服务器目录", "Glama MCP server directory") },
    { name: "pulsemcp.com", url: "https://www.pulsemcp.com", description: () => t("Pulse MCP 服务器", "Pulse MCP servers") },
    { name: "mcp.composio.dev", url: "https://mcp.composio.dev/", description: () => t("Composio MCP 开发工具", "Composio MCP developer tooling") },
    { name: "Model Context Protocol Servers", url: "https://github.com/modelcontextprotocol/servers", description: () => t("官方 MCP 服务器集合", "The official MCP server collection") },
    { name: "Awesome MCP Servers", url: "https://github.com/wong2/awesome-mcp-servers", description: () => t("精选的 MCP 服务器列表", "A curated list of MCP servers") }
  ];

  return (
    <div className="mcp-market">
      <h2 className="mcp-pane__title">{t("更多 MCP", "Find more MCP servers")}</h2>
      <p className="mcp-market__lead">
        {t(
          "这些站点收录了可以配置进来的 MCP 服务器。挑好之后回到「MCP 服务器」页添加，或直接粘贴它给出的 JSON 配置。",
          "These directories list MCP servers you can configure here. Add one from the MCP servers page, or paste the JSON configuration it gives you."
        )}
      </p>
      <div className="mcp-market__grid">
        {markets.map((market) => (
          <a
            className="mcp-market__card"
            key={market.name}
            href={market.url}
            target="_blank"
            rel="noreferrer noopener"
          >
            <ProviderAvatar name={market.name} />
            <span className="mcp-market__copy">
              <span className="mcp-market__name">
                {market.name}
                <ExternalLink size={13} aria-hidden="true" />
              </span>
              <small>{market.description()}</small>
            </span>
          </a>
        ))}
      </div>
    </div>
  );
}
