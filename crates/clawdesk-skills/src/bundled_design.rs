//! Bundled design skills for the Design Agent runbook.
//!
//! Six design-oriented skills that ship with ClawDesk:
//!
//! | Skill ID              | Purpose                                     |
//! |-----------------------|---------------------------------------------|
//! | `wireframe`           | Generate lo-fi wireframe descriptions       |
//! | `component-spec`      | Produce component specification documents   |
//! | `accessibility-audit` | Check designs against WCAG 2.1 guidelines   |
//! | `design-system`       | Create/maintain a design token system       |
//! | `user-flow`           | Map user flows and interaction sequences    |
//! | `design-critique`     | Provide structured design feedback           |

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ---------------------------------------------------------------------------
// Design skill definitions
// ---------------------------------------------------------------------------

/// A bundled design skill definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DesignSkill {
    /// Unique skill ID.
    pub id: String,
    /// Human-readable name.
    pub display_name: String,
    /// Description.
    pub description: String,
    /// Trigger phrases.
    pub triggers: Vec<String>,
    /// Required tools.
    pub tools: Vec<String>,
    /// Skill prompt template.
    pub prompt: String,
    /// Output format hint.
    pub output_format: String,
    /// Tags.
    pub tags: Vec<String>,
}

/// Return all bundled design skills.
pub fn bundled_design_skills() -> Vec<DesignSkill> {
    vec![
        DesignSkill {
            id: "wireframe".into(),
            display_name: "Wireframe Generator".into(),
            description: "Generate low-fidelity wireframe descriptions from requirements".into(),
            triggers: vec![
                "wireframe".into(),
                "mockup".into(),
                "layout".into(),
                "page structure".into(),
            ],
            tools: vec!["read_file".into(), "write_file".into()],
            prompt: indoc("
                You are a UX wireframing specialist. Given a feature description,
                produce a detailed lo-fi wireframe specification including:

                1. **Page Layout** — grid structure, sections, responsive breakpoints
                2. **Component Inventory** — every UI element with placeholder content
                3. **Interaction Notes** — hover states, click targets, transitions
                4. **Accessibility** — landmark roles, tab order, ARIA labels

                Use ASCII art or structured markdown to represent the layout.
                Be specific about spacing, alignment, and visual hierarchy.
            "),
            output_format: "markdown".into(),
            tags: vec!["design".into(), "ux".into(), "wireframe".into()],
        },
        DesignSkill {
            id: "component-spec".into(),
            display_name: "Component Spec Writer".into(),
            description: "Produce detailed component specification documents".into(),
            triggers: vec![
                "component spec".into(),
                "component specification".into(),
                "component api".into(),
            ],
            tools: vec!["read_file".into(), "write_file".into(), "search_files".into()],
            prompt: indoc("
                You are a UI component architect. Given a component name and context,
                produce a complete specification including:

                1. **Props/API** — every prop with type, default, and description
                2. **States** — all visual states (default, hover, active, disabled, loading, error)
                3. **Variants** — size, colour, and style variants
                4. **Composition** — how it composes with other components
                5. **Accessibility** — ARIA roles, keyboard navigation, screen reader behaviour
                6. **Design Tokens** — colours, spacing, typography tokens used
                7. **Examples** — usage examples in code

                Output as structured markdown with TypeScript type definitions.
            "),
            output_format: "markdown".into(),
            tags: vec!["design".into(), "component".into(), "spec".into()],
        },
        DesignSkill {
            id: "accessibility-audit".into(),
            display_name: "Accessibility Auditor".into(),
            description: "Check designs and code against WCAG 2.1 AA guidelines".into(),
            triggers: vec![
                "accessibility".into(),
                "a11y".into(),
                "wcag".into(),
                "screen reader".into(),
            ],
            tools: vec!["read_file".into(), "search_files".into()],
            prompt: indoc("
                You are a WCAG 2.1 AA accessibility expert. Audit the provided
                design or code and produce a structured report:

                1. **Perceivable** — colour contrast, text alternatives, captions
                2. **Operable** — keyboard access, focus indicators, timing
                3. **Understandable** — labels, error messages, consistent navigation
                4. **Robust** — semantic HTML, ARIA usage, assistive tech compatibility

                For each issue:
                - Severity: Critical / Major / Minor
                - WCAG criterion reference (e.g. 1.4.3)
                - Current state
                - Recommended fix
                - Code example of the fix

                Rate overall compliance: Pass / Partial / Fail.
            "),
            output_format: "markdown".into(),
            tags: vec!["design".into(), "accessibility".into(), "audit".into()],
        },
        DesignSkill {
            id: "design-system".into(),
            display_name: "Design System Manager".into(),
            description: "Create and maintain a design token system".into(),
            triggers: vec![
                "design system".into(),
                "design tokens".into(),
                "style guide".into(),
                "theme".into(),
            ],
            tools: vec!["read_file".into(), "write_file".into(), "search_files".into()],
            prompt: indoc("
                You are a design systems engineer. Manage the project's design
                token system:

                1. **Colours** — semantic palette (primary, secondary, surface, error, etc.)
                2. **Typography** — font families, scale, weights, line heights
                3. **Spacing** — spacing scale (4px base grid)
                4. **Elevation** — shadow definitions
                5. **Motion** — easing curves, duration tokens
                6. **Breakpoints** — responsive breakpoint tokens
                7. **Border** — radius and width tokens

                Output tokens in multiple formats:
                - CSS custom properties
                - Tailwind config
                - JSON token file (Design Tokens Community Group format)

                Ensure tokens are semantic (not raw values), consistent, and documented.
            "),
            output_format: "json+markdown".into(),
            tags: vec!["design".into(), "tokens".into(), "system".into()],
        },
        DesignSkill {
            id: "user-flow".into(),
            display_name: "User Flow Mapper".into(),
            description: "Map user flows and interaction sequences".into(),
            triggers: vec![
                "user flow".into(),
                "user journey".into(),
                "interaction flow".into(),
                "workflow".into(),
            ],
            tools: vec!["read_file".into(), "write_file".into()],
            prompt: indoc("
                You are a UX flow analyst. Map the user flow for the given feature:

                1. **Entry Points** — how users arrive at this flow
                2. **Happy Path** — ideal step-by-step sequence
                3. **Decision Points** — branches and conditional paths
                4. **Error Paths** — what happens when things go wrong
                5. **Exit Points** — where users leave the flow
                6. **Edge Cases** — unusual but valid scenarios

                Represent the flow as:
                - A numbered step list with decision branches
                - A Mermaid flowchart diagram
                - Touch points with other systems/features

                Note estimated user time for each step.
            "),
            output_format: "markdown+mermaid".into(),
            tags: vec!["design".into(), "ux".into(), "flow".into()],
        },
        DesignSkill {
            id: "design-critique".into(),
            display_name: "Design Critic".into(),
            description: "Provide structured design feedback and improvement suggestions".into(),
            triggers: vec![
                "critique".into(),
                "review design".into(),
                "design feedback".into(),
                "improve design".into(),
            ],
            tools: vec!["read_file".into()],
            prompt: indoc("
                You are a senior design critic. Review the provided design and give
                structured feedback:

                1. **First Impression** — gut reaction, visual appeal, clarity
                2. **Hierarchy** — information architecture, visual weight, flow
                3. **Consistency** — alignment with design system, pattern usage
                4. **Usability** — learnability, efficiency, error prevention
                5. **Accessibility** — inclusive design considerations
                6. **Polish** — spacing, alignment, attention to detail

                For each area:
                - Score: 1-5
                - Strengths (what works well)
                - Improvements (specific, actionable suggestions)

                End with a prioritised action list: Quick Wins | Important | Nice-to-Have.
            "),
            output_format: "markdown".into(),
            tags: vec!["design".into(), "critique".into(), "review".into()],
        },
    ]
}

/// Helper: trim common leading whitespace from a multi-line string.
fn indoc(s: &str) -> String {
    let lines: Vec<&str> = s.lines().collect();
    let min_indent = lines
        .iter()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.len() - l.trim_start().len())
        .min()
        .unwrap_or(0);

    lines
        .iter()
        .map(|l| {
            if l.len() >= min_indent {
                &l[min_indent..]
            } else {
                l.trim()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

/// Get a design skill by ID.
pub fn get_design_skill(id: &str) -> Option<DesignSkill> {
    bundled_design_skills().into_iter().find(|s| s.id == id)
}

/// List all design skill IDs.
pub fn design_skill_ids() -> Vec<String> {
    bundled_design_skills().iter().map(|s| s.id.clone()).collect()
}

/// Generate a TOML manifest for a design skill.
pub fn design_skill_manifest_toml(skill: &DesignSkill) -> String {
    let triggers = skill
        .triggers
        .iter()
        .map(|t| format!("\"{}\"", t))
        .collect::<Vec<_>>()
        .join(", ");

    let tools = skill
        .tools
        .iter()
        .map(|t| format!("\"{}\"", t))
        .collect::<Vec<_>>()
        .join(", ");

    let tags = skill
        .tags
        .iter()
        .map(|t| format!("\"{}\"", t))
        .collect::<Vec<_>>()
        .join(", ");

    format!(
        r#"[skill]
id = "{id}"
display_name = "{name}"
description = "{desc}"
version = "1.0.0"
author = "clawdesk"

[skill.triggers]
phrases = [{triggers}]

[skill.tools]
required = [{tools}]

[skill.metadata]
output_format = "{fmt}"
tags = [{tags}]
category = "design"
bundled = true
"#,
        id = skill.id,
        name = skill.display_name,
        desc = skill.description,
        triggers = triggers,
        tools = tools,
        fmt = skill.output_format,
        tags = tags,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bundled_count() {
        assert_eq!(bundled_design_skills().len(), 6);
    }

    #[test]
    fn test_all_ids_unique() {
        let ids = design_skill_ids();
        let unique: std::collections::HashSet<_> = ids.iter().collect();
        assert_eq!(ids.len(), unique.len());
    }

    #[test]
    fn test_get_wireframe() {
        let skill = get_design_skill("wireframe").unwrap();
        assert_eq!(skill.display_name, "Wireframe Generator");
        assert!(!skill.triggers.is_empty());
        assert!(!skill.prompt.is_empty());
    }

    #[test]
    fn test_get_nonexistent() {
        assert!(get_design_skill("nonexistent").is_none());
    }

    #[test]
    fn test_all_have_prompts() {
        for skill in bundled_design_skills() {
            assert!(!skill.prompt.is_empty(), "Skill {} has empty prompt", skill.id);
        }
    }

    #[test]
    fn test_all_have_triggers() {
        for skill in bundled_design_skills() {
            assert!(!skill.triggers.is_empty(), "Skill {} has no triggers", skill.id);
        }
    }

    #[test]
    fn test_manifest_generation() {
        let skill = get_design_skill("component-spec").unwrap();
        let manifest = design_skill_manifest_toml(&skill);
        assert!(manifest.contains("component-spec"));
        assert!(manifest.contains("bundled = true"));
        assert!(manifest.contains("category = \"design\""));
    }

    #[test]
    fn test_indoc() {
        let result = indoc("
            line one
            line two
        ");
        assert!(result.starts_with("line one"));
        assert!(result.contains("line two"));
    }

    #[test]
    fn test_expected_skill_ids() {
        let ids = design_skill_ids();
        assert!(ids.contains(&"wireframe".to_string()));
        assert!(ids.contains(&"component-spec".to_string()));
        assert!(ids.contains(&"accessibility-audit".to_string()));
        assert!(ids.contains(&"design-system".to_string()));
        assert!(ids.contains(&"user-flow".to_string()));
        assert!(ids.contains(&"design-critique".to_string()));
    }
}
