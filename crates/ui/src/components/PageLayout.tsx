import { ReactNode } from "react";

interface PageLayoutProps {
    title: string;
    subtitle?: string;
    actions?: ReactNode;
    children: ReactNode;
    className?: string;
}

export function PageLayout({ title, subtitle, actions, children, className = "" }: PageLayoutProps) {
    return (
        <div className={`view ${className}`} style={{ height: "100%", display: "flex", flexDirection: "column" }}>
            <header className="page-header">
                <div>
                    <h1 className="page-title">{title}</h1>
                    {subtitle && <p className="page-subtitle">{subtitle}</p>}
                </div>
                {actions && <div className="page-header-actions">{actions}</div>}
            </header>

            <div style={{ flex: 1, minHeight: 0, overflowY: "auto", display: "flex", flexDirection: "column" }}>
                {children}
            </div>
        </div>
    );
}
