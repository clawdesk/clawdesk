//! Internationalization (i18n) — multi-language support with fluent-style patterns.
//!
//! Provides a zero-allocation message catalog backed by static string maps.
//! Supports variable interpolation and pluralization.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::RwLock;

/// Supported locales.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Locale {
    En,
    Es,
    Fr,
    De,
    Ja,
    Zh,
    Ko,
    Pt,
    Ar,
    Hi,
}

impl Locale {
    pub fn code(&self) -> &'static str {
        match self {
            Self::En => "en",
            Self::Es => "es",
            Self::Fr => "fr",
            Self::De => "de",
            Self::Ja => "ja",
            Self::Zh => "zh",
            Self::Ko => "ko",
            Self::Pt => "pt",
            Self::Ar => "ar",
            Self::Hi => "hi",
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            Self::En => "English",
            Self::Es => "Español",
            Self::Fr => "Français",
            Self::De => "Deutsch",
            Self::Ja => "日本語",
            Self::Zh => "中文",
            Self::Ko => "한국어",
            Self::Pt => "Português",
            Self::Ar => "العربية",
            Self::Hi => "हिन्दी",
        }
    }

    pub fn direction(&self) -> TextDirection {
        match self {
            Self::Ar => TextDirection::Rtl,
            _ => TextDirection::Ltr,
        }
    }

    pub fn from_code(code: &str) -> Option<Self> {
        match code {
            "en" => Some(Self::En),
            "es" => Some(Self::Es),
            "fr" => Some(Self::Fr),
            "de" => Some(Self::De),
            "ja" => Some(Self::Ja),
            "zh" => Some(Self::Zh),
            "ko" => Some(Self::Ko),
            "pt" => Some(Self::Pt),
            "ar" => Some(Self::Ar),
            "hi" => Some(Self::Hi),
            _ => None,
        }
    }

    pub fn all() -> &'static [Locale] {
        &[
            Self::En, Self::Es, Self::Fr, Self::De, Self::Ja,
            Self::Zh, Self::Ko, Self::Pt, Self::Ar, Self::Hi,
        ]
    }
}

/// Text direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TextDirection {
    Ltr,
    Rtl,
}

/// Message catalog — holds translations for a locale.
pub type MessageCatalog = HashMap<String, String>;

/// Internationalization context.
pub struct I18n {
    active_locale: RwLock<Locale>,
    catalogs: HashMap<Locale, MessageCatalog>,
    fallback: Locale,
}

impl I18n {
    /// Create a new i18n context with English as fallback.
    pub fn new() -> Self {
        let mut catalogs = HashMap::new();
        catalogs.insert(Locale::En, Self::english_catalog());
        Self {
            active_locale: RwLock::new(Locale::En),
            catalogs,
            fallback: Locale::En,
        }
    }

    /// Create i18n context with all built-in catalogs loaded.
    pub fn with_all_catalogs() -> Self {
        let mut catalogs = HashMap::new();
        catalogs.insert(Locale::En, Self::english_catalog());
        catalogs.insert(Locale::Es, Self::spanish_catalog());
        catalogs.insert(Locale::Zh, Self::chinese_catalog());
        catalogs.insert(Locale::Ja, Self::japanese_catalog());
        Self {
            active_locale: RwLock::new(Locale::En),
            catalogs,
            fallback: Locale::En,
        }
    }

    /// Set the active locale.
    pub fn set_locale(&self, locale: Locale) {
        if let Ok(mut active) = self.active_locale.write() {
            *active = locale;
        }
    }

    /// Get the active locale.
    pub fn locale(&self) -> Locale {
        self.active_locale.read().map(|l| *l).unwrap_or(self.fallback)
    }

    /// Add a catalog for a locale.
    pub fn add_catalog(&mut self, locale: Locale, catalog: MessageCatalog) {
        self.catalogs.insert(locale, catalog);
    }

    /// Translate a key.
    pub fn t(&self, key: &str) -> String {
        let locale = self.locale();
        if let Some(catalog) = self.catalogs.get(&locale) {
            if let Some(msg) = catalog.get(key) {
                return msg.clone();
            }
        }
        // Fallback
        if let Some(catalog) = self.catalogs.get(&self.fallback) {
            if let Some(msg) = catalog.get(key) {
                return msg.clone();
            }
        }
        // Return key itself if not found
        key.to_string()
    }

    /// Translate with variable substitution.
    /// Variables use `{name}` syntax.
    pub fn t_with(&self, key: &str, vars: &[(&str, &str)]) -> String {
        let mut result = self.t(key);
        for (name, value) in vars {
            result = result.replace(&format!("{{{}}}", name), value);
        }
        result
    }

    /// Default English catalog.
    fn english_catalog() -> MessageCatalog {
        let mut c = HashMap::new();
        // Navigation
        c.insert("nav.chat".into(), "Chat".into());
        c.insert("nav.agents".into(), "Agents".into());
        c.insert("nav.skills".into(), "Skills".into());
        c.insert("nav.settings".into(), "Settings".into());
        c.insert("nav.help".into(), "Help".into());
        // Chat
        c.insert("chat.placeholder".into(), "Type a message...".into());
        c.insert("chat.send".into(), "Send".into());
        c.insert("chat.thinking".into(), "Thinking...".into());
        c.insert("chat.clear".into(), "Clear conversation".into());
        c.insert("chat.copy".into(), "Copy".into());
        c.insert("chat.regenerate".into(), "Regenerate".into());
        // Agents
        c.insert("agent.create".into(), "Create Agent".into());
        c.insert("agent.delete".into(), "Delete".into());
        c.insert("agent.name".into(), "Agent Name".into());
        c.insert("agent.model".into(), "Model".into());
        c.insert("agent.prompt".into(), "System Prompt".into());
        // Status
        c.insert("status.connected".into(), "Connected".into());
        c.insert("status.disconnected".into(), "Disconnected".into());
        c.insert("status.error".into(), "Error".into());
        c.insert("status.loading".into(), "Loading...".into());
        // Errors
        c.insert("error.network".into(), "Network error — check your connection".into());
        c.insert("error.auth".into(), "Authentication failed".into());
        c.insert("error.rate_limit".into(), "Rate limit exceeded — please wait".into());
        c.insert("error.generic".into(), "Something went wrong".into());
        // Settings
        c.insert("settings.theme".into(), "Theme".into());
        c.insert("settings.language".into(), "Language".into());
        c.insert("settings.api_keys".into(), "API Keys".into());
        c.insert("settings.save".into(), "Save".into());
        c.insert("settings.cancel".into(), "Cancel".into());
        // Canvas
        c.insert("canvas.title".into(), "Canvas".into());
        c.insert("canvas.new_block".into(), "New Block".into());
        c.insert("canvas.export".into(), "Export".into());
        // Token counts
        c.insert("tokens.used".into(), "{count} tokens used".into());
        c.insert("tokens.remaining".into(), "{count} tokens remaining".into());
        c
    }

    /// Spanish (Español) catalog.
    fn spanish_catalog() -> MessageCatalog {
        let mut c = HashMap::new();
        c.insert("nav.chat".into(), "Chat".into());
        c.insert("nav.agents".into(), "Agentes".into());
        c.insert("nav.skills".into(), "Habilidades".into());
        c.insert("nav.settings".into(), "Configuración".into());
        c.insert("nav.help".into(), "Ayuda".into());
        c.insert("chat.placeholder".into(), "Escribe un mensaje...".into());
        c.insert("chat.send".into(), "Enviar".into());
        c.insert("chat.thinking".into(), "Pensando...".into());
        c.insert("chat.clear".into(), "Limpiar conversación".into());
        c.insert("chat.copy".into(), "Copiar".into());
        c.insert("chat.regenerate".into(), "Regenerar".into());
        c.insert("agent.create".into(), "Crear Agente".into());
        c.insert("agent.delete".into(), "Eliminar".into());
        c.insert("agent.name".into(), "Nombre del Agente".into());
        c.insert("agent.model".into(), "Modelo".into());
        c.insert("agent.prompt".into(), "Prompt del Sistema".into());
        c.insert("status.connected".into(), "Conectado".into());
        c.insert("status.disconnected".into(), "Desconectado".into());
        c.insert("status.error".into(), "Error".into());
        c.insert("status.loading".into(), "Cargando...".into());
        c.insert("error.network".into(), "Error de red — verifica tu conexión".into());
        c.insert("error.auth".into(), "Autenticación fallida".into());
        c.insert("error.rate_limit".into(), "Límite de velocidad excedido — espera un momento".into());
        c.insert("error.generic".into(), "Algo salió mal".into());
        c.insert("settings.theme".into(), "Tema".into());
        c.insert("settings.language".into(), "Idioma".into());
        c.insert("settings.api_keys".into(), "Claves API".into());
        c.insert("settings.save".into(), "Guardar".into());
        c.insert("settings.cancel".into(), "Cancelar".into());
        c.insert("canvas.title".into(), "Lienzo".into());
        c.insert("canvas.new_block".into(), "Nuevo Bloque".into());
        c.insert("canvas.export".into(), "Exportar".into());
        c.insert("tokens.used".into(), "{count} tokens usados".into());
        c.insert("tokens.remaining".into(), "{count} tokens restantes".into());
        c
    }

    /// Chinese (中文) catalog.
    fn chinese_catalog() -> MessageCatalog {
        let mut c = HashMap::new();
        c.insert("nav.chat".into(), "聊天".into());
        c.insert("nav.agents".into(), "代理".into());
        c.insert("nav.skills".into(), "技能".into());
        c.insert("nav.settings".into(), "设置".into());
        c.insert("nav.help".into(), "帮助".into());
        c.insert("chat.placeholder".into(), "输入消息...".into());
        c.insert("chat.send".into(), "发送".into());
        c.insert("chat.thinking".into(), "思考中...".into());
        c.insert("chat.clear".into(), "清除对话".into());
        c.insert("chat.copy".into(), "复制".into());
        c.insert("chat.regenerate".into(), "重新生成".into());
        c.insert("agent.create".into(), "创建代理".into());
        c.insert("agent.delete".into(), "删除".into());
        c.insert("agent.name".into(), "代理名称".into());
        c.insert("agent.model".into(), "模型".into());
        c.insert("agent.prompt".into(), "系统提示".into());
        c.insert("status.connected".into(), "已连接".into());
        c.insert("status.disconnected".into(), "已断开".into());
        c.insert("status.error".into(), "错误".into());
        c.insert("status.loading".into(), "加载中...".into());
        c.insert("error.network".into(), "网络错误 — 请检查连接".into());
        c.insert("error.auth".into(), "认证失败".into());
        c.insert("error.rate_limit".into(), "请求频率超限 — 请稍候".into());
        c.insert("error.generic".into(), "出了点问题".into());
        c.insert("settings.theme".into(), "主题".into());
        c.insert("settings.language".into(), "语言".into());
        c.insert("settings.api_keys".into(), "API 密钥".into());
        c.insert("settings.save".into(), "保存".into());
        c.insert("settings.cancel".into(), "取消".into());
        c.insert("canvas.title".into(), "画布".into());
        c.insert("canvas.new_block".into(), "新建块".into());
        c.insert("canvas.export".into(), "导出".into());
        c.insert("tokens.used".into(), "已使用 {count} 个令牌".into());
        c.insert("tokens.remaining".into(), "剩余 {count} 个令牌".into());
        c
    }

    /// Japanese (日本語) catalog.
    fn japanese_catalog() -> MessageCatalog {
        let mut c = HashMap::new();
        c.insert("nav.chat".into(), "チャット".into());
        c.insert("nav.agents".into(), "エージェント".into());
        c.insert("nav.skills".into(), "スキル".into());
        c.insert("nav.settings".into(), "設定".into());
        c.insert("nav.help".into(), "ヘルプ".into());
        c.insert("chat.placeholder".into(), "メッセージを入力...".into());
        c.insert("chat.send".into(), "送信".into());
        c.insert("chat.thinking".into(), "考え中...".into());
        c.insert("chat.clear".into(), "会話をクリア".into());
        c.insert("chat.copy".into(), "コピー".into());
        c.insert("chat.regenerate".into(), "再生成".into());
        c.insert("agent.create".into(), "エージェントを作成".into());
        c.insert("agent.delete".into(), "削除".into());
        c.insert("agent.name".into(), "エージェント名".into());
        c.insert("agent.model".into(), "モデル".into());
        c.insert("agent.prompt".into(), "システムプロンプト".into());
        c.insert("status.connected".into(), "接続済み".into());
        c.insert("status.disconnected".into(), "切断".into());
        c.insert("status.error".into(), "エラー".into());
        c.insert("status.loading".into(), "読み込み中...".into());
        c.insert("error.network".into(), "ネットワークエラー — 接続を確認してください".into());
        c.insert("error.auth".into(), "認証に失敗しました".into());
        c.insert("error.rate_limit".into(), "レート制限超過 — しばらくお待ちください".into());
        c.insert("error.generic".into(), "問題が発生しました".into());
        c.insert("settings.theme".into(), "テーマ".into());
        c.insert("settings.language".into(), "言語".into());
        c.insert("settings.api_keys".into(), "APIキー".into());
        c.insert("settings.save".into(), "保存".into());
        c.insert("settings.cancel".into(), "キャンセル".into());
        c.insert("canvas.title".into(), "キャンバス".into());
        c.insert("canvas.new_block".into(), "新しいブロック".into());
        c.insert("canvas.export".into(), "エクスポート".into());
        c.insert("tokens.used".into(), "{count} トークン使用".into());
        c.insert("tokens.remaining".into(), "{count} トークン残り".into());
        c
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn translate_existing_key() {
        let i18n = I18n::new();
        assert_eq!(i18n.t("nav.chat"), "Chat");
    }

    #[test]
    fn translate_missing_key_returns_key() {
        let i18n = I18n::new();
        assert_eq!(i18n.t("nonexistent.key"), "nonexistent.key");
    }

    #[test]
    fn translate_with_vars() {
        let i18n = I18n::new();
        let result = i18n.t_with("tokens.used", &[("count", "1500")]);
        assert_eq!(result, "1500 tokens used");
    }

    #[test]
    fn locale_from_code() {
        assert_eq!(Locale::from_code("en"), Some(Locale::En));
        assert_eq!(Locale::from_code("ja"), Some(Locale::Ja));
        assert_eq!(Locale::from_code("xx"), None);
    }

    #[test]
    fn rtl_detection() {
        assert_eq!(Locale::Ar.direction(), TextDirection::Rtl);
        assert_eq!(Locale::En.direction(), TextDirection::Ltr);
    }

    #[test]
    fn all_locales() {
        assert_eq!(Locale::all().len(), 10);
    }

    #[test]
    fn locale_switching() {
        let mut i18n = I18n::new();
        let mut es = HashMap::new();
        es.insert("nav.chat".into(), "Chat (ES)".into());
        i18n.add_catalog(Locale::Es, es);

        i18n.set_locale(Locale::Es);
        assert_eq!(i18n.t("nav.chat"), "Chat (ES)");
        // Fallback to English for missing keys
        assert_eq!(i18n.t("nav.agents"), "Agents");
    }

    #[test]
    fn all_catalogs_spanish() {
        let i18n = I18n::with_all_catalogs();
        i18n.set_locale(Locale::Es);
        assert_eq!(i18n.t("chat.send"), "Enviar");
        assert_eq!(i18n.t("nav.settings"), "Configuración");
    }

    #[test]
    fn all_catalogs_chinese() {
        let i18n = I18n::with_all_catalogs();
        i18n.set_locale(Locale::Zh);
        assert_eq!(i18n.t("chat.send"), "发送");
        assert_eq!(i18n.t("nav.agents"), "代理");
    }

    #[test]
    fn all_catalogs_japanese() {
        let i18n = I18n::with_all_catalogs();
        i18n.set_locale(Locale::Ja);
        assert_eq!(i18n.t("chat.send"), "送信");
        assert_eq!(i18n.t("nav.skills"), "スキル");
    }

    #[test]
    fn all_catalogs_fallback_to_english() {
        let i18n = I18n::with_all_catalogs();
        // French not loaded, should fall back to English
        i18n.set_locale(Locale::Fr);
        assert_eq!(i18n.t("chat.send"), "Send");
    }
}
