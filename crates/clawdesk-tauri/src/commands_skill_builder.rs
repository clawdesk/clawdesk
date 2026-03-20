//! Tauri commands for the No-Code Visual Skill Builder (Phase 2.2).
//!
//! Exposes the visual DAG editor and skill compilation pipeline.

use crate::state::AppState;
use clawdesk_skills::visual_builder::{
    NodeTemplate, SkillBuilderOutput, ValidationError, VisualDAG,
    available_templates,
};
use serde::Deserialize;
use tauri::State;

/// Get all available node templates for the drag-and-drop palette.
#[tauri::command]
pub async fn skill_builder_get_templates() -> Result<Vec<NodeTemplate>, String> {
    Ok(available_templates())
}

/// Validate a visual DAG and return any errors.
#[tauri::command]
pub async fn skill_builder_validate(
    dag: VisualDAG,
) -> Result<Vec<ValidationError>, String> {
    Ok(dag.validate())
}

/// Compile a visual DAG into a skill (TOML + prompt template).
#[tauri::command]
pub async fn skill_builder_compile(
    dag: VisualDAG,
    state: State<'_, AppState>,
) -> Result<SkillBuilderOutput, String> {
    dag.compile_to_scaffold_input()
        .map_err(|errors| {
            let msgs: Vec<String> = errors.iter().map(|e| e.message.clone()).collect();
            format!("Validation errors: {}", msgs.join("; "))
        })
}

/// Compile and install a visual DAG as a live skill.
#[tauri::command]
pub async fn skill_builder_deploy(
    dag: VisualDAG,
    state: State<'_, AppState>,
) -> Result<SkillBuilderOutput, String> {
    let output = dag.compile_to_scaffold_input()
        .map_err(|errors| {
            let msgs: Vec<String> = errors.iter().map(|e| e.message.clone()).collect();
            format!("Validation errors: {}", msgs.join("; "))
        })?;

    // Generate the scaffold files
    let base_dir = clawdesk_types::dirs::data().join("skills");
    let input = clawdesk_skills::scaffold::SkillScaffoldInput {
        id: output.skill_id.clone(),
        display_name: output.name.clone(),
        description: output.description.clone(),
        triggers: output.trigger_descriptions.clone(),
        tools: output.tools.clone(),
        parameters: Vec::new(),
        author: "Visual Builder".to_string(),
        version: "1.0.0".to_string(),
        dependencies: Vec::new(),
        tags: vec!["visual-builder".to_string()],
    };

    let scaffold = clawdesk_skills::scaffold::generate_scaffold(&input, &base_dir);

    // Write skill files to disk
    let skill_dir = &scaffold.skill_dir;
    std::fs::create_dir_all(skill_dir)
        .map_err(|e| format!("Failed to create skill dir: {}", e))?;
    std::fs::write(skill_dir.join("skill.toml"), &scaffold.manifest_toml)
        .map_err(|e| format!("Failed to write skill.toml: {}", e))?;
    std::fs::write(skill_dir.join("prompt.md"), &scaffold.prompt_md)
        .map_err(|e| format!("Failed to write prompt.md: {}", e))?;

    // Store DAG JSON for re-editing
    std::fs::write(skill_dir.join("dag.json"), &output.dag_json)
        .map_err(|e| format!("Failed to write dag.json: {}", e))?;

    // Hot-reload into registry
    // (The skill watcher will pick up the new files automatically)

    Ok(output)
}
