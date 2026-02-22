import { useMemo, useState, useCallback } from "react";
import type { PipelineDescriptor, PipelineNodeDescriptor, PipelineStepEvent } from "../types";

// ── Types ─────────────────────────────────────────────────────

interface DagCanvasProps {
  pipeline: PipelineDescriptor;
  /** Live execution state for step overlay (T8) */
  stepEvents?: Map<number, PipelineStepEvent>;
  width?: number;
  height?: number;
  onNodeClick?: (index: number, node: PipelineNodeDescriptor) => void;
}

interface LayoutNode {
  index: number;
  node: PipelineNodeDescriptor;
  x: number;
  y: number;
  layer: number;
}

// ── Layout Engine ─────────────────────────────────────────────

/** Simple layered (Sugiyama-style) DAG layout.
 *  Assigns layers via topological sort, then spaces nodes evenly. */
function layoutDag(
  nodes: PipelineNodeDescriptor[],
  edges: [number, number][],
  canvasW: number,
  canvasH: number,
): { lnodes: LayoutNode[]; ledges: [LayoutNode, LayoutNode][] } {
  const n = nodes.length;
  if (n === 0) return { lnodes: [], ledges: [] };

  // Build adjacency & in-degree
  const adj: number[][] = Array.from({ length: n }, () => []);
  const inDeg = new Array(n).fill(0);
  for (const [u, v] of edges) {
    if (u < n && v < n) {
      adj[u].push(v);
      inDeg[v]++;
    }
  }

  // Topological sort → assign layers
  const layer = new Array(n).fill(0);
  const queue: number[] = [];
  for (let i = 0; i < n; i++) {
    if (inDeg[i] === 0) queue.push(i);
  }
  const order: number[] = [];
  while (queue.length > 0) {
    const u = queue.shift()!;
    order.push(u);
    for (const v of adj[u]) {
      layer[v] = Math.max(layer[v], layer[u] + 1);
      if (--inDeg[v] === 0) queue.push(v);
    }
  }
  // Include any nodes not reached (isolated)
  for (let i = 0; i < n; i++) {
    if (!order.includes(i)) order.push(i);
  }

  const maxLayer = Math.max(...layer, 0);
  const layerGroups: number[][] = Array.from({ length: maxLayer + 1 }, () => []);
  for (const i of order) {
    layerGroups[layer[i]].push(i);
  }

  // Assign positions
  const PAD_X = 80;
  const PAD_Y = 60;
  const usableW = canvasW - PAD_X * 2;
  const usableH = canvasH - PAD_Y * 2;
  const layerSpacing = maxLayer > 0 ? usableW / maxLayer : 0;

  const lnodes: LayoutNode[] = [];
  for (let l = 0; l <= maxLayer; l++) {
    const group = layerGroups[l];
    const groupSpacing = group.length > 1 ? usableH / (group.length - 1) : 0;
    for (let gi = 0; gi < group.length; gi++) {
      const idx = group[gi];
      lnodes.push({
        index: idx,
        node: nodes[idx],
        x: PAD_X + l * layerSpacing,
        y: PAD_Y + (group.length === 1 ? usableH / 2 : gi * groupSpacing),
        layer: l,
      });
    }
  }

  // Sort by index for consistent lookup
  const byIndex = new Map<number, LayoutNode>();
  for (const ln of lnodes) byIndex.set(ln.index, ln);

  const ledges: [LayoutNode, LayoutNode][] = [];
  for (const [u, v] of edges) {
    const lu = byIndex.get(u);
    const lv = byIndex.get(v);
    if (lu && lv) ledges.push([lu, lv]);
  }

  return { lnodes, ledges };
}

// ── Node Colors ───────────────────────────────────────────────

function nodeColor(nodeType: string, stepEvent?: PipelineStepEvent): string {
  if (stepEvent) {
    switch (stepEvent.status) {
      case "started": return "#3b82f6";  // blue — running
      case "completed": return "#22c55e"; // green
      case "failed": return "#ef4444";    // red
    }
  }
  switch (nodeType) {
    case "input": return "#6366f1";    // indigo
    case "output": return "#8b5cf6";   // purple
    case "agent": return "#10b981";    // emerald
    case "gate": return "#f59e0b";     // amber
    case "parallel": return "#06b6d4"; // cyan
    default: return "#6b7280";         // gray
  }
}

function nodeIcon(nodeType: string): string {
  switch (nodeType) {
    case "input": return "\u25B6";   // ▶
    case "output": return "\u25C0";   // ◀
    case "agent": return "\uD83E\uDD16";   // 🤖
    case "gate": return "\uD83D\uDD00";    // 🔀
    case "parallel": return "\u2261"; // ≡
    default: return "\u25CF";         // ●
  }
}

// ── Component ─────────────────────────────────────────────────

const NODE_W = 140;
const NODE_H = 48;

export function DagCanvas({
  pipeline,
  stepEvents,
  width = 800,
  height = 400,
  onNodeClick,
}: DagCanvasProps) {
  const [hoveredNode, setHoveredNode] = useState<number | null>(null);

  const { lnodes, ledges } = useMemo(
    () => layoutDag(pipeline.steps, pipeline.edges, width, height),
    [pipeline.steps, pipeline.edges, width, height],
  );

  const handleNodeClick = useCallback(
    (index: number, node: PipelineNodeDescriptor) => {
      onNodeClick?.(index, node);
    },
    [onNodeClick],
  );

  if (pipeline.steps.length === 0) {
    return (
      <div style={{ width, height, display: "flex", alignItems: "center", justifyContent: "center", color: "var(--text-tertiary)", fontSize: 14 }}>
        No steps in this pipeline.
      </div>
    );
  }

  return (
    <svg
      width={width}
      height={height}
      style={{ background: "var(--bg-secondary)", borderRadius: 8, border: "1px solid var(--border)" }}
    >
      <defs>
        <marker
          id="arrowhead"
          viewBox="0 0 10 7"
          refX="10"
          refY="3.5"
          markerWidth="8"
          markerHeight="7"
          orient="auto"
        >
          <polygon points="0 0, 10 3.5, 0 7" fill="var(--text-tertiary)" />
        </marker>
        {/* Glow filter for active nodes */}
        <filter id="glow">
          <feGaussianBlur stdDeviation="3" result="blur" />
          <feMerge>
            <feMergeNode in="blur" />
            <feMergeNode in="SourceGraphic" />
          </feMerge>
        </filter>
      </defs>

      {/* Edges */}
      {ledges.map(([from, to], i) => {
        const x1 = from.x + NODE_W / 2;
        const y1 = from.y + NODE_H / 2;
        const x2 = to.x + NODE_W / 2;
        const y2 = to.y + NODE_H / 2;
        // Cubic bezier for smooth curved edge
        const cx1 = x1 + (x2 - x1) * 0.5;
        const cy1 = y1;
        const cx2 = x2 - (x2 - x1) * 0.5;
        const cy2 = y2;

        // Edge from right side of source to left side of target
        const sx = from.x + NODE_W;
        const sy = from.y + NODE_H / 2;
        const tx = to.x;
        const ty = to.y + NODE_H / 2;
        const scx = sx + (tx - sx) * 0.4;
        const tcx = tx - (tx - sx) * 0.4;

        return (
          <path
            key={i}
            d={`M ${sx} ${sy} C ${scx} ${sy}, ${tcx} ${ty}, ${tx} ${ty}`}
            fill="none"
            stroke="var(--text-tertiary)"
            strokeWidth={1.5}
            strokeDasharray={stepEvents?.get(from.index)?.status === "completed" ? "none" : "4 3"}
            markerEnd="url(#arrowhead)"
            opacity={0.6}
          />
        );
      })}

      {/* Nodes */}
      {lnodes.map((ln) => {
        const ev = stepEvents?.get(ln.index);
        const color = nodeColor(ln.node.node_type, ev);
        const isHovered = hoveredNode === ln.index;
        const isActive = ev?.status === "started";

        return (
          <g
            key={ln.index}
            transform={`translate(${ln.x}, ${ln.y})`}
            style={{ cursor: "pointer" }}
            onMouseEnter={() => setHoveredNode(ln.index)}
            onMouseLeave={() => setHoveredNode(null)}
            onClick={() => handleNodeClick(ln.index, ln.node)}
            filter={isActive ? "url(#glow)" : undefined}
          >
            {/* Node background */}
            <rect
              width={NODE_W}
              height={NODE_H}
              rx={8}
              fill={isHovered ? color : `${color}22`}
              stroke={color}
              strokeWidth={isActive ? 2.5 : 1.5}
            />

            {/* Icon circle */}
            <circle
              cx={20}
              cy={NODE_H / 2}
              r={12}
              fill={`${color}30`}
              stroke={color}
              strokeWidth={1}
            />
            <text
              x={20}
              y={NODE_H / 2}
              textAnchor="middle"
              dominantBaseline="central"
              fontSize={12}
            >
              {nodeIcon(ln.node.node_type)}
            </text>

            {/* Label */}
            <text
              x={40}
              y={NODE_H / 2 - 6}
              fill={isHovered ? "white" : "var(--text-primary)"}
              fontSize={12}
              fontWeight={500}
            >
              {ln.node.label.length > 12 ? ln.node.label.substring(0, 12) + "\u2026" : ln.node.label}
            </text>

            {/* Type tag */}
            <text
              x={40}
              y={NODE_H / 2 + 10}
              fill={isHovered ? "rgba(255,255,255,0.7)" : "var(--text-tertiary)"}
              fontSize={10}
            >
              {ln.node.node_type}
              {ln.node.model ? ` \u00b7 ${ln.node.model}` : ""}
            </text>

            {/* Execution status indicator */}
            {ev && (
              <circle
                cx={NODE_W - 12}
                cy={12}
                r={5}
                fill={color}
              >
                {isActive && (
                  <animate
                    attributeName="opacity"
                    values="1;0.3;1"
                    dur="1.5s"
                    repeatCount="indefinite"
                  />
                )}
              </circle>
            )}
          </g>
        );
      })}
    </svg>
  );
}
