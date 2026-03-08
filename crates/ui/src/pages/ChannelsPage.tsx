import { useState, useEffect, useCallback } from "react";
import * as api from "../api";
import { ChannelSetupJourney } from "../onboarding/ChannelSetupJourney";
import type { ChannelInfo, ChannelTypeSpec } from "../types";
import { PageLayout } from "../components/PageLayout";

export interface ChannelsPageProps {
  channels: ChannelInfo[];
  onRefreshChannels: () => void;
  pushToast: (text: string) => void;
}

export function ChannelsPage({
  channels,
  onRefreshChannels,
  pushToast,
}: ChannelsPageProps) {
  const [typeSpecs, setTypeSpecs] = useState<ChannelTypeSpec[]>([]);
  const [configuring, setConfiguring] = useState<ChannelInfo | null>(null);
  const [saving, setSaving] = useState(false);

  useEffect(() => {
    api.getChannelTypes().then(setTypeSpecs).catch(() => {});
  }, []);

  const specFor = useCallback(
    (ch: ChannelInfo) => typeSpecs.find((ts) => ts.id === ch.channel_type),
    [typeSpecs]
  );

  const openConfig = useCallback((ch: ChannelInfo) => {
    setConfiguring(ch);
  }, []);

  const disconnect = useCallback(async (ch: ChannelInfo) => {
    try {
      await api.disconnectChannel(ch.id);
      pushToast(`${ch.name} disconnected.`);
      onRefreshChannels();
    } catch {
      pushToast(`Failed to disconnect ${ch.name}.`);
    }
  }, [onRefreshChannels, pushToast]);

  const connected = channels.filter((c) => c.status === "active" || c.status === "configured");
  const available = channels.filter((c) => c.status === "available");
  const spec = configuring ? specFor(configuring) : null;
  const activeConnected = connected.filter((c) => c.status === "active").length;
  const capabilityCount = new Set(channels.flatMap((channel) => channel.capabilities ?? [])).size;

  return (
    <PageLayout
      title="Channels"
      subtitle="Connect messaging platforms. ClawDesk normalizes inbound messages and renders outbound responses automatically."
      className="page-channels"
      actions={
        <button className="btn subtle" onClick={onRefreshChannels}>
          Refresh
        </button>
      }
    >
      <div className="channels-page-content">
        <section className="channels-hero">
          <div className="channels-hero__intro">
            <span className="channels-hero__eyebrow">Messaging network</span>
            <h2>Keep inbound and outbound channels connected, healthy, and easy to configure.</h2>
            <p>{connected.length} connected, {activeConnected} active now, and {capabilityCount} exposed channel capabilities across the workspace.</p>
          </div>
          <div className="channels-hero__stats">
            <ChannelHeroStat label="Connected" value={connected.length.toString()} meta="Saved or live integrations" />
            <ChannelHeroStat label="Active" value={activeConnected.toString()} meta="Currently receiving traffic" />
            <ChannelHeroStat label="Available" value={available.length.toString()} meta="Ready to connect" />
          </div>
        </section>

        {connected.length > 0 && (
          <div className="settings-group channels-section-card">
            <div className="settings-group-label">Connected</div>
            <div className="channel-grid">
              {connected.map((ch) => {
                const ts = specFor(ch);
                const isRunning = ch.status === "active";
                return (
                  <div key={ch.id} className={`channel-card ${isRunning ? "channel-card--active" : ""}`}>
                    <div className="channel-card-info">
                      <h3>
                        {ts?.icon ?? "📡"} {ch.name}
                      </h3>
                      <p>
                        {ch.channel_type} · {isRunning
                          ? <span className="status-text-ok">active</span>
                          : <span className="status-text-warn" title="Config saved — will connect on next start or after reconnect">configured</span>
                        }
                      </p>
                      {(ch.capabilities ?? []).length > 0 && (
                        <div className="channel-card-caps">
                          {(ch.capabilities ?? []).map((c) => (
                            <span key={c} className="chip chip-sm">{c}</span>
                          ))}
                        </div>
                      )}
                    </div>
                    <span className={`status-dot ${isRunning ? "status-ok" : "status-warn"}`} />
                    <div className="channel-card-btns">
                      <button className="btn subtle" onClick={() => openConfig(ch)}>Configure</button>
                      {ch.channel_type !== "WebChat" && ch.channel_type !== "Internal" && (
                        <button className="btn ghost" onClick={() => disconnect(ch)}>Disconnect</button>
                      )}
                    </div>
                  </div>
                );
              })}
            </div>
          </div>
        )}

        {available.length > 0 && (
          <div className="settings-group channels-section-card">
            <div className="settings-group-label">Available</div>
            <div className="channel-grid">
              {available.map((ch) => {
                const ts = specFor(ch);
                return (
                  <div key={ch.id} className="channel-card">
                    <div className="channel-card-info">
                      <h3>
                        {ts?.icon ?? "📡"} {ch.name}
                      </h3>
                      <p>{ch.channel_type} · <span className="status-text-off">available</span></p>
                    </div>
                    <span className="status-dot status-error" />
                    <button className="btn subtle" onClick={() => openConfig(ch)}>Connect</button>
                  </div>
                );
              })}
            </div>
          </div>
        )}

        {channels.length === 0 && (
          <div className="empty-state">
            <p>No channels found.</p>
            <button className="btn primary" onClick={onRefreshChannels}>Refresh</button>
          </div>
        )}
      </div>

      {configuring && spec && (
        <div className="modal-backdrop" onClick={() => setConfiguring(null)}>
          <div className="modal channel-config-modal" onClick={(e) => e.stopPropagation()}>
            <div className="modal-head">
              <h2>{spec.icon} {configuring.name} Setup</h2>
              <button className="btn ghost" onClick={() => setConfiguring(null)}>✕</button>
            </div>
            <div className="modal-body">
              <ChannelSetupJourney
                spec={spec}
                initialValues={configuring.config ?? {}}
                onComplete={async (config) => {
                  setSaving(true);
                  try {
                    await api.updateChannel(configuring.id, config);
                    pushToast(`${configuring.name} connected.`);
                    onRefreshChannels();
                    setConfiguring(null);
                  } catch {
                    pushToast(`Failed to save ${configuring.name} config.`);
                  } finally {
                    setSaving(false);
                  }
                }}
                onCancel={() => setConfiguring(null)}
              />
            </div>
          </div>
        </div>
      )}
    </PageLayout>
  );
}

function ChannelHeroStat({ label, value, meta }: { label: string; value: string; meta: string }) {
  return (
    <div className="channels-hero-stat">
      <span>{label}</span>
      <strong>{value}</strong>
      <small>{meta}</small>
    </div>
  );
}
