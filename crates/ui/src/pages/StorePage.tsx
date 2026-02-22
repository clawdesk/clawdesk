import { useState, useCallback, useMemo, useRef, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { PageLayout } from "../components/PageLayout";
import { Icon } from "../components/Icon";

// ── Types (mirrors store.rs) ──────────────────────────────────

export type InstallState =
  | "available"
  | "installing"
  | "installed"
  | "active"
  | "update_available"
  | "failed";

export interface StoreEntry {
  skill_id: string;
  display_name: string;
  version: string;
  description: string;
  author: string;
  category: string;
  tags: string[];
  rating: number;
  install_count: number;
  verified: boolean;
  install_state: InstallState;
  icon: string;
  // Trust chain fields
  trust_level?: "unsigned" | "signed(untrusted)" | "signed(trusted)" | "builtin";
  publisher_key?: string;
  content_hash?: string;
  dependencies?: string[];
}

export type SortOrder = "relevance" | "popularity" | "rating" | "newest";

// ── Category definitions (mirrors StoreCategory in store.rs) ──

const CATEGORIES = [
  { id: "all", label: "All", icon: "🏠" },
  { id: "coding", label: "Coding", icon: "💻" },
  { id: "writing", label: "Writing", icon: "✍️" },
  { id: "productivity", label: "Productivity", icon: "📋" },
  { id: "data_analysis", label: "Data Analysis", icon: "📊" },
  { id: "communication", label: "Communication", icon: "💬" },
  { id: "design", label: "Design", icon: "🎨" },
  { id: "devops", label: "DevOps", icon: "🔧" },
  { id: "security", label: "Security", icon: "🔒" },
  { id: "education", label: "Education", icon: "📚" },
  { id: "fun", label: "Fun", icon: "🎮" },
  { id: "other", label: "Other", icon: "📦" },
] as const;

// ── Props ─────────────────────────────────────────────────────

export interface StorePageProps {
  pushToast: (text: string) => void;
}

// ── Star Rating ───────────────────────────────────────────────

function StarRating({ rating }: { rating: number }) {
  const full = Math.floor(rating);
  const half = rating - full >= 0.5;
  const empty = 5 - full - (half ? 1 : 0);

  return (
    <span className="store-stars" title={`${rating.toFixed(1)} / 5`}>
      {"★".repeat(full)}
      {half && "½"}
      {"☆".repeat(empty)}
    </span>
  );
}

// ── Install badge ─────────────────────────────────────────────

function InstallBadge({ state }: { state: InstallState }) {
  switch (state) {
    case "installed":
    case "active":
      return <span className="store-badge store-badge-installed">Installed</span>;
    case "installing":
      return <span className="store-badge store-badge-installing">Installing…</span>;
    case "update_available":
      return <span className="store-badge store-badge-update">Update</span>;
    case "failed":
      return <span className="store-badge store-badge-failed">Failed</span>;
    default:
      return null;
  }
}

// ── Trust badge ───────────────────────────────────────────────

function TrustBadge({ trustLevel, contentHash }: {
  trustLevel?: string;
  contentHash?: string;
}) {
  if (!trustLevel || trustLevel === "unsigned") return null;

  const label = trustLevel === "builtin"
    ? "🔒 Builtin"
    : trustLevel === "signed(trusted)"
      ? "✓ Signed"
      : "⚠ Untrusted";

  const className = trustLevel === "builtin" || trustLevel === "signed(trusted)"
    ? "store-badge store-badge-trusted"
    : "store-badge store-badge-untrusted";

  return (
    <span
      className={className}
      title={contentHash ? `Content: ${contentHash.slice(0, 16)}…` : ""}
    >
      {label}
    </span>
  );
}

// ── Store Card ────────────────────────────────────────────────

interface StoreCardProps {
  entry: StoreEntry;
  onInstall: (id: string) => void;
  onUninstall: (id: string) => void;
}

function StoreCard({ entry, onInstall, onUninstall }: StoreCardProps) {
  const isInstalled = entry.install_state === "installed" || entry.install_state === "active";
  const isInstalling = entry.install_state === "installing";

  return (
    <div className="store-card">
      <div className="store-card-header">
        <span className="store-card-icon">{entry.icon || "📦"}</span>
        <div className="store-card-title-block">
          <h3 className="store-card-name">
            {entry.display_name}
            {entry.verified && <span className="store-verified" title="Verified">✓</span>}
          </h3>
          <span className="store-card-author">by {entry.author}</span>
        </div>
        <InstallBadge state={entry.install_state} />
        <TrustBadge trustLevel={entry.trust_level} contentHash={entry.content_hash} />
      </div>

      <p className="store-card-desc">{entry.description}</p>

      <div className="store-card-meta">
        <StarRating rating={entry.rating} />
        <span className="store-card-installs">
          {entry.install_count.toLocaleString()} installs
        </span>
        <span className="store-card-version">v{entry.version}</span>
        {entry.dependencies && entry.dependencies.length > 0 && (
          <span className="store-card-deps" title={entry.dependencies.join(", ")}>
            {entry.dependencies.length} dep{entry.dependencies.length > 1 ? "s" : ""}
          </span>
        )}
      </div>

      <div className="store-card-tags">
        {entry.tags.slice(0, 3).map((tag) => (
          <span key={tag} className="store-tag">{tag}</span>
        ))}
      </div>

      <div className="store-card-actions">
        {isInstalled ? (
          <button
            className="btn subtle store-uninstall-btn"
            onClick={() => onUninstall(entry.skill_id)}
          >
            Uninstall
          </button>
        ) : (
          <button
            className="btn primary store-install-btn"
            disabled={isInstalling}
            onClick={() => onInstall(entry.skill_id)}
          >
            {isInstalling ? "Installing…" : "Install"}
          </button>
        )}
      </div>
    </div>
  );
}

// ── Main StorePage ────────────────────────────────────────────

const PAGE_SIZE = 24;

export function StorePage({ pushToast }: StorePageProps) {
  const [searchQuery, setSearchQuery] = useState("");
  const [selectedCategory, setSelectedCategory] = useState("all");
  const [sortOrder, setSortOrder] = useState<SortOrder>("popularity");
  const [verifiedOnly, setVerifiedOnly] = useState(false);
  const [entries, setEntries] = useState<StoreEntry[]>([]);
  const [loading, setLoading] = useState(false);
  const [loadingMore, setLoadingMore] = useState(false);
  const [totalResults, setTotalResults] = useState(0);
  const [currentPage, setCurrentPage] = useState(0);
  const [hasMore, setHasMore] = useState(false);

  // Intersection observer for infinite scroll
  const sentinelRef = useRef<HTMLDivElement>(null);

  const displayEntries = useMemo(() => {
    if (entries.length > 0) return entries;
    return [] as StoreEntry[];
  }, [entries]);

  const handleSearch = useCallback(async (resetPage = true) => {
    const page = resetPage ? 0 : currentPage;
    if (resetPage) {
      setLoading(true);
    } else {
      setLoadingMore(true);
    }

    try {
      const result = await invoke<{ entries: StoreEntry[]; total: number }>(
        "search_store",
        {
          query: searchQuery || null,
          category: selectedCategory === "all" ? null : selectedCategory,
          sortBy: sortOrder,
          verifiedOnly,
          page,
          pageSize: PAGE_SIZE,
        }
      );

      if (resetPage) {
        setEntries(result.entries);
        setCurrentPage(1);
      } else {
        setEntries((prev) => [...prev, ...result.entries]);
        setCurrentPage((prev) => prev + 1);
      }

      setTotalResults(result.total);
      setHasMore(
        (resetPage ? result.entries.length : entries.length + result.entries.length) < result.total
      );
    } catch (err) {
      console.error("Store search failed:", err);
      pushToast("Failed to search the skill store");
    } finally {
      setLoading(false);
      setLoadingMore(false);
    }
  }, [searchQuery, selectedCategory, sortOrder, verifiedOnly, pushToast, currentPage, entries.length]);

  // Infinite scroll via IntersectionObserver
  useEffect(() => {
    if (!sentinelRef.current) return;
    const observer = new IntersectionObserver(
      (observerEntries) => {
        const [entry] = observerEntries;
        if (entry.isIntersecting && hasMore && !loading && !loadingMore) {
          handleSearch(false);
        }
      },
      { rootMargin: "200px" }
    );
    observer.observe(sentinelRef.current);
    return () => observer.disconnect();
  }, [hasMore, loading, loadingMore, handleSearch]);

  const handleInstall = useCallback(
    async (skillId: string) => {
      try {
        await invoke("install_store_skill", { skillId });
        pushToast(`Installing ${skillId}…`);
        handleSearch(true);
      } catch (err) {
        console.error("Install failed:", err);
        pushToast(`Failed to install ${skillId}`);
      }
    },
    [pushToast, handleSearch]
  );

  const handleUninstall = useCallback(
    async (skillId: string) => {
      try {
        await invoke("uninstall_store_skill", { skillId });
        pushToast(`Uninstalled ${skillId}`);
        handleSearch(true);
      } catch (err) {
        console.error("Uninstall failed:", err);
        pushToast(`Failed to uninstall ${skillId}`);
      }
    },
    [pushToast, handleSearch]
  );

  return (
    <PageLayout title="Skill Store">
      {/* ── Search & Filters ── */}
      <div className="store-toolbar">
        <div className="store-search-bar">
          <input
            type="text"
            className="store-search-input"
            placeholder="Search skills…"
            value={searchQuery}
            onChange={(e) => setSearchQuery(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && handleSearch()}
          />
          <button
            className="btn primary store-search-btn"
            onClick={() => { void handleSearch(); }}
            disabled={loading}
          >
            {loading ? "Searching…" : "Search"}
          </button>
        </div>

        <div className="store-filters">
          <select
            className="store-sort-select"
            value={sortOrder}
            onChange={(e) => setSortOrder(e.target.value as SortOrder)}
          >
            <option value="relevance">Relevance</option>
            <option value="popularity">Most Popular</option>
            <option value="rating">Highest Rated</option>
            <option value="newest">Newest</option>
          </select>

          <label className="store-verified-filter">
            <input
              type="checkbox"
              checked={verifiedOnly}
              onChange={(e) => setVerifiedOnly(e.target.checked)}
            />
            Verified only
          </label>
        </div>
      </div>

      {/* ── Category sidebar + Results ── */}
      <div className="store-layout">
        <nav className="store-categories">
          {CATEGORIES.map((cat) => (
            <button
              key={cat.id}
              className={`store-category-btn ${selectedCategory === cat.id ? "active" : ""
                }`}
              onClick={() => {
                setSelectedCategory(cat.id);
                handleSearch();
              }}
            >
              <span className="store-category-icon">{cat.icon}</span>
              {cat.label}
            </button>
          ))}
        </nav>

        <div className="store-results">
          {loading && (
            <div className="store-loading">
              <span className="spinner" /> Loading…
            </div>
          )}

          {!loading && displayEntries.length === 0 && (
            <div className="store-empty">
              <p>No skills found. Try a different search or category.</p>
              <button className="btn subtle" onClick={() => { void handleSearch(); }}>
                Browse all skills
              </button>
            </div>
          )}

          <div className="store-grid">
            {displayEntries.map((entry) => (
              <StoreCard
                key={entry.skill_id}
                entry={entry}
                onInstall={handleInstall}
                onUninstall={handleUninstall}
              />
            ))}
          </div>

          {totalResults > displayEntries.length && (
            <div className="store-pagination">
              <span>{displayEntries.length} of {totalResults} results</span>
              {loadingMore && (
                <span className="store-loading-more">
                  <span className="spinner" /> Loading more…
                </span>
              )}
            </div>
          )}

          {/* Infinite scroll sentinel */}
          <div ref={sentinelRef} className="store-scroll-sentinel" />
        </div>
      </div>
    </PageLayout>
  );
}

export default StorePage;
