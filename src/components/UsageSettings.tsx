import { useEffect, useState } from "react";
import { useI18n } from "../i18n";
import {
  EMPTY_USAGE_STATISTICS,
  ensureUsageBackfill,
  fetchUsageStatistics,
  type UsageStatistics
} from "../lib/usageStatistics";
import type { AppDocument } from "../types";
import { UsageStatsCard } from "./UsageStatsCard";

/**
 * Usage statistics page. It owns its own read because it is the only surface
 * that shows the ledger, and the backfill has to run wherever that read happens.
 */
export function UsageSettings({ document }: { document: AppDocument | null }) {
  const { t } = useI18n();
  const [statistics, setStatistics] = useState<UsageStatistics>(EMPTY_USAGE_STATISTICS);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    void (async () => {
      try {
        const read = await fetchUsageStatistics();
        if (!cancelled) setStatistics(read);
        // Pre-gateway usage survives only in the renderer's local turn table; backfill based on
        // the ledger's accounting start rather than a resettable flag.
        if (await ensureUsageBackfill(document, read)) {
          const refreshed = await fetchUsageStatistics();
          if (!cancelled) setStatistics(refreshed);
        }
      } catch (error) {
        console.error(t("读取使用统计失败", "Failed to read usage statistics"), error);
      } finally {
        if (!cancelled) setLoading(false);
      }
    })();
    return () => { cancelled = true; };
    // The document is only read to backfill turns recorded before the ledger existed, so a
    // later edit to it must not refetch.
  }, []);

  return <UsageStatsCard statistics={statistics} loading={loading} />;
}
