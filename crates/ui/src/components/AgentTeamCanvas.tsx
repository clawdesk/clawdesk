import { useMemo, useState, useCallback } from "react";

// ── Types ─────────────────────────────────────────────────────

export interface TeamNode {
  id: string;
  label: string;
  icon: string;
  color: string;
  role: string;
  /** "leader" | "member" */
  kind: "leader" | "member";
}

export interface TeamEdge {
  from: string;
  to: string;
  label?: string;
}

export interface AgentTeamCanvasProps {
  nodes: TeamNode[];
  edges: TeamEdge[];
  width?: number;
  height?: number;
  onNodeClick?: (node: TeamNode) => void;
  onAddNode?: () => void;
  onRemoveNode?: (id: string) => void;
  interactive?: boolean;
}

// ── Layout Engine (Sugiyama-inspired radial) ──────────────────

interface LayoutNode extends TeamNode {
  x: number;
  y: number;
  layer: number;
}

function layoutTeam(
  nodes: TeamNode[],
  edges: TeamEdge[],
  canvasW: number,
  canvasH: number,
): { lnodes: LayoutNode[]; ledges: { from: LayoutNode; to: LayoutNode; label?: string }[] } {
  if (nodes.length === 0) return { lnodes: [], ledges: [] };

  // Build adjacency from edges
  const childMap = new Map<string, string[]>();
  const parentSet = new Set<string>();
  for (const e of edges) {
    const kids = childMap.get(e.from) || [];
    kids.push(e.to);
    childMap.set(e.from, kids);
    parentSet.add(e.to);
  }

  // Find root (leader) nodes
  const roots = nodes.filter((n) => n.kind === "leader" || !parentSet.has(n.id));
  const nonRoots = nodes.filter((n) => !roots.includes(n));

  // Assign layers via BFS
  const layerMap = new Map<string, number>();
  const queue: string[] = [];
  for (const r of roots) {
    layerMap.set(r.id, 0);
    queue.push(r.id);
  }
  while (queue.length > 0) {
    const curr = queue.shift()!;
    const currLayer = layerMap.get(curr) || 0;
    for (const child of childMap.get(curr) || []) {
      if (!layerMap.has(child)) {
        layerMap.set(child, currLayer + 1);
        queue.push(child);
      }
    }
  }
  // Assign any orphans
  for (const n of nodes) {
    if (!layerMap.has(n.id)) layerMap.set(n.id, 1);
  }

  const maxLayer = Math.max(...Array.from(layerMap.values()), 0);
  const layerGroups: TeamNode[][] = Array.from({ length: maxLayer + 1 }, () => []);
  for (const n of nodes) {
    layerGroups[layerMap.get(n.id) || 0].push(n);
  }

  // Position nodes
  const PAD_X = 80;
  const PAD_Y = 60;
  const usableW = canvasW - PAD_X * 2;
  const usableH = canvasH - PAD_Y * 2;
  const layerSpacing = maxLayer > 0 ? usableH / maxLayer : 0;

  const lnodes: LayoutNode[] = [];
  const byId = new Map<string, LayoutNode>();

  for (let l = 0; l <= maxLayer; l++) {
    const group = layerGroups[l];
    const groupSpacing = group.length > 1 ? usableW / (group.length - 1) : 0;
    for (let gi = 0; gi < group.length; gi++) {
      const n = group[gi];
      const ln: LayoutNode = {
        ...n,
        x: PAD_X + (group.length === 1 ? usableW / 2 : gi * groupSpacing),
        y: PAD_Y + l * layerSpacing,
        layer: l,
      };
      lnodes.push(ln);
      byId.set(n.id, ln);
    }
  }

  const ledges: { from: LayoutNode; to: LayoutNode; label?: string }[] = [];
  for (const e of edges) {
    const from = byId.get(e.from);
    const to = byId.get(e.to);
    if (from && to) ledges.push({ from, to, label: e.label });
  }

  return { lnodes, ledges };
}

// ── Constants ─────────────────────────────────────────────────

const NODE_W = 160;
const NODE_H = 56;
const LEADER_W = 180;
const LEADER_H = 64;

// ── Component ─────────────────────────────────────────────────

export function AgentTeamCanvas({
  nodes,
  edges,
  width = 700,
  height = 360,
  onNodeClick,
  onAddNode,
  onRemoveNode,
  interactive = true,
}: AgentTeamCanvasProps) {
  const [hoveredNode, setHoveredNode] = useState<string | null>(null);

  const { lnodes, ledges } = useMemo(
    () => layoutTeam(nodes, edges, width, height),
    [nodes, edges, width, height],
  );

  const handleNodeClick = useCallback(
    (node: TeamNode) => onNodeClick?.(node),
    [onNodeClick],
  );

  if (nodes.length === 0) {
    return (
      <div
        className="team-canvas-empty"
        style={{
          width,
          height,
          display: "flex",
          flexDirection: "column",
          alignItems: "center",
          justifyContent: "center",
          background: "var(--bg-secondary)",
          borderRadius: 12,
          border: "2px dashed var(--border)",
          gap: 12,
        }}
      >
        <span style={{ fontSize: 32, opacity: 0.5 }}>🔗</span>
        <span style={{ color: "var(--text-tertiary)", fontSize: 13 }}>
          No team members yet
        </span>
        {onAddNode && (
          <button className="btn subtle" style={{ fontSize: 12 }} onClick={onAddNode}>
            + Add Agent
          </button>
        )}
      </div>
    );
  }

  return (
    <div style={{ position: "relative" }}>
      <svg
        width={width}
        height={height}
        style={{
          background: "var(--bg-secondary)",
          borderRadius: 12,
          border: "1px solid var(--border)",
        }}
      >
        <defs>
          <marker
            id="team-arrow"
            viewBox="0 0 10 7"
            refX="10"
            refY="3.5"
            markerWidth="8"
            markerHeight="7"
            orient="auto"
          >
            <polygon points="0 0, 10 3.5, 0 7" fill="var(--text-tertiary)" opacity={0.6} />
          </marker>
          <filter id="node-shadow">
            <feDropShadow dx="0" dy="2" stdDeviation="4" floodOpacity="0.15" />
          </filter>
          <filter id="leader-glow">
            <feGaussianBlur stdDeviation="6" result="blur" />
            <feMerge>
              <feMergeNode in="blur" />
              <feMergeNode in="SourceGraphic" />
            </feMerge>
          </filter>
        </defs>

        {/* Edges */}
        {ledges.map((edge, i) => {
          const fromW = edge.from.kind === "leader" ? LEADER_W : NODE_W;
          const fromH = edge.from.kind === "leader" ? LEADER_H : NODE_H;
          const toW = edge.to.kind === "leader" ? LEADER_W : NODE_W;
          const toH = edge.to.kind === "leader" ? LEADER_H : NODE_H;

          const sx = edge.from.x + fromW / 2;
          const sy = edge.from.y + fromH;
          const tx = edge.to.x + toW / 2;
          const ty = edge.to.y;

          // Smooth bezier
          const midY = (sy + ty) / 2;

          return (
            <g key={i}>
              <path
                d={`M ${sx} ${sy} C ${sx} ${midY}, ${tx} ${midY}, ${tx} ${ty}`}
                fill="none"
                stroke={edge.from.color || "var(--text-tertiary)"}
                strokeWidth={2}
                strokeDasharray="6 3"
                markerEnd="url(#team-arrow)"
                opacity={0.5}
              />
              {edge.label && (
                <text
                  x={(sx + tx) / 2}
                  y={midY - 6}
                  textAnchor="middle"
                  fill="var(--text-tertiary)"
                  fontSize={10}
                  fontWeight={500}
                >
                  {edge.label}
                </text>
              )}
            </g>
          );
        })}

        {/* Nodes */}
        {lnodes.map((ln) => {
          const isLeader = ln.kind === "leader";
          const w = isLeader ? LEADER_W : NODE_W;
          const h = isLeader ? LEADER_H : NODE_H;
          const isHovered = hoveredNode === ln.id;

          return (
            <g
              key={ln.id}
              transform={`translate(${ln.x}, ${ln.y})`}
              style={{ cursor: interactive ? "pointer" : "default" }}
              onMouseEnter={() => setHoveredNode(ln.id)}
              onMouseLeave={() => setHoveredNode(null)}
              onClick={() => interactive && handleNodeClick(ln)}
              filter={isLeader ? "url(#leader-glow)" : "url(#node-shadow)"}
            >
              {/* Node background */}
              <rect
                width={w}
                height={h}
                rx={isLeader ? 14 : 10}
                fill={isHovered ? `${ln.color}40` : `${ln.color}18`}
                stroke={ln.color}
                strokeWidth={isLeader ? 2.5 : 1.5}
              />

              {/* Icon circle */}
              <circle
                cx={isLeader ? 24 : 22}
                cy={h / 2}
                r={isLeader ? 16 : 14}
                fill={`${ln.color}30`}
                stroke={ln.color}
                strokeWidth={1}
              />
              <text
                x={isLeader ? 24 : 22}
                y={h / 2}
                textAnchor="middle"
                dominantBaseline="central"
                fontSize={isLeader ? 16 : 14}
              >
                {ln.icon}
              </text>

              {/* Label */}
              <text
                x={isLeader ? 48 : 44}
                y={h / 2 - (ln.role ? 6 : 0)}
                fill={isHovered ? "var(--text)" : "var(--text-primary)"}
                fontSize={isLeader ? 14 : 12}
                fontWeight={isLeader ? 700 : 500}
              >
                {ln.label.length > 14
                  ? ln.label.substring(0, 14) + "\u2026"
                  : ln.label}
              </text>

              {/* Role tag */}
              {ln.role && (
                <text
                  x={isLeader ? 48 : 44}
                  y={h / 2 + 12}
                  fill="var(--text-tertiary)"
                  fontSize={10}
                >
                  {ln.role.length > 18 ? ln.role.substring(0, 18) + "\u2026" : ln.role}
                </text>
              )}

              {/* Leader crown */}
              {isLeader && (
                <text
                  x={w - 16}
                  y={14}
                  fontSize={12}
                  textAnchor="middle"
                >
                  👑
                </text>
              )}

              {/* Remove button on hover */}
              {interactive && isHovered && onRemoveNode && !isLeader && (
                <g
                  onClick={(e) => {
                    e.stopPropagation();
                    onRemoveNode(ln.id);
                  }}
                >
                  <circle cx={w - 8} cy={8} r={8} fill="#ef4444" opacity={0.9} />
                  <text
                    x={w - 8}
                    y={9}
                    textAnchor="middle"
                    dominantBaseline="central"
                    fill="white"
                    fontSize={10}
                    fontWeight={700}
                  >
                    ×
                  </text>
                </g>
              )}
            </g>
          );
        })}
      </svg>

      {/* Add button overlay */}
      {interactive && onAddNode && (
        <button
          onClick={onAddNode}
          style={{
            position: "absolute",
            bottom: 12,
            right: 12,
            width: 36,
            height: 36,
            borderRadius: "50%",
            border: "2px dashed var(--border)",
            background: "var(--surface)",
            cursor: "pointer",
            display: "flex",
            alignItems: "center",
            justifyContent: "center",
            fontSize: 18,
            color: "var(--text-tertiary)",
            transition: "all 0.2s",
          }}
          title="Add team member"
        >
          +
        </button>
      )}
    </div>
  );
}
