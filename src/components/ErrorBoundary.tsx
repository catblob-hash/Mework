import { Component, type ErrorInfo, type ReactNode } from "react";
import { t } from "../i18n";

export class CommonErrorBoundary extends Component<{ children: ReactNode }, { error: Error | null }> {
  state: { error: Error | null } = { error: null };

  static getDerivedStateFromError(error: Error) {
    return { error };
  }

  componentDidCatch(error: Error, info: ErrorInfo) {
    console.error("Mework render error", error, info.componentStack);
  }

  render() {
    if (this.state.error) {
      return (
        <div className="fatal-state">
          <h1>{t("界面遇到了问题", "Something went wrong with the interface")}</h1>
          <p>{this.state.error.message}</p>
          <button type="button" className="button button--primary" onClick={() => window.location.reload()}>{t("重新载入", "Reload")}</button>
        </div>
      );
    }
    return this.props.children;
  }
}
