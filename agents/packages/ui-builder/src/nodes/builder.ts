import fs from "node:fs";
import {
    readFileTool,
    writeFileTool,
    editFileTool,
    screenshotTool,
} from "@dimension-agents/shared";
import type { NodeConfig, NodeResult, GraphState } from "@dimension-agents/shared";

const systemPrompt = fs.readFileSync(
    new URL("../../src/prompts/builder.md", import.meta.url),
    "utf-8",
);

/**
 * Builder node: takes the plan (and optional feedback) and writes HTML/CSS/JS code.
 * Uses GPT-4.1 (default model) with filesystem + screenshot tools.
 */
export const builderNode: NodeConfig = {
    id: "builder",
    systemPrompt,
    tools: [readFileTool, writeFileTool, editFileTool, screenshotTool],
    // Uses the graph's default model (GPT-4.1)

    buildPrompt(state: GraphState): string {
        const parts: string[] = [];

        // Include the plan
        const plan = state.artifacts["plan"];
        if (plan) {
            parts.push("## Implementation Plan\n\n" + plan);
        }

        // Include reviewer feedback if this is a revision cycle
        const lastReviewerMessage = [...state.messages]
            .reverse()
            .find(m => m.node === "reviewer");
        if (lastReviewerMessage) {
            parts.push("## Reviewer Feedback\n\n" + lastReviewerMessage.content);
            parts.push(
                "Please revise the HTML at /app/workspace/ui-output.html based on this feedback. " +
                "After making changes, take a new screenshot."
            );
        } else {
            parts.push(
                "Build the UI as a single HTML file at /app/workspace/ui-output.html. " +
                "After writing the file, take a screenshot of it."
            );
        }

        return parts.join("\n\n");
    },

    parseResult(output: string, _state: GraphState): NodeResult {
        // Try to extract screenshot base64 from the output
        // The screenshot tool returns base64 — the agent may have included it
        // We store it as an artifact for the reviewer
        const artifacts: Record<string, string> = {};

        // Look for a large base64 blob in the output (screenshot result)
        const base64Match = output.match(/([A-Za-z0-9+/]{1000,}={0,2})/);
        if (base64Match) {
            artifacts["built_screenshot"] = base64Match[1];
        }

        artifacts["builder_output"] = output;

        return {
            output,
            artifacts,
        };
    },
};
