import { ReactNode } from "react";
import { Icon } from "./Icon";

interface PageLayoutProps {
    title: string;
    subtitle?: string;
    actions?: ReactNode;
    children: ReactNode;
    className?: string;
    onRefresh?: () => void;
    loading?: boolean;
}

export function PageLayout({ title, subtitle, actions, children, className = "", onRefresh, loading = false }: PageLayoutProps) {
    return (
        <div className={`view ${className}`} style={{ height: "100%", display: "flex", flexDirection: "column" }}>
            <header className="page-header">
                <div>
                    <h1 className="page-title">{title}</h1>
                    {subtitle && <p className="page-subtitle">{subtitle}</p>}
                </div>
                {(actions || onRefresh) && (
                    <div className="page-header-actions">
                        {onRefresh ? (
                            <button className="btn subtle" onClick={onRefresh} disabled={loading}>
                                <Icon name="refresh" className={loading ? "spin" : ""} />
                                {loading ? "Refreshing..." : "Refresh"}
                            </button>
                        ) : null}
                        {actions}
                    </div>
                )}
            </header>

            <div style={{ flex: 1, minHeight: 0, overflowY: "auto", display: "flex", flexDirection: "column" }}>
                {children}
            </div>
        </div>
    );
}
