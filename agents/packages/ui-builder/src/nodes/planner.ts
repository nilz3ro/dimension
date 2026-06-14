import fs from "node:fs";
import { createCompatModel } from "@dimension-agents/shared";
import type { NodeConfig, NodeResult, GraphState } from "@dimension-agents/shared";

const systemPrompt = fs.readFileSync(
    new URL("../../src/prompts/planner.md", import.meta.url),
    "utf-8",
);

const kimiModel = createCompatModel({
    id: "kimi-k2.5",
    name: "Kimi K2.5",
    baseUrl: "https://api.moonshot.cn/v1",
    provider: "moonshot",
    apiKeyEnvVariable: "MOONSHOT_API_KEY",
    reasoning: true,
    input: ["text", "image"],
    contextWindow: 262144,
    maxTokens: 262144,
});

/**
 * Planner node: analyzes the input screenshot/description using Kimi k2.5 vision
 * and produces a detailed implementation plan.
 */
export const plannerNode: NodeConfig = {
    id: "planner",
    systemPrompt,
    tools: [], // Planner only analyzes and outputs text — no tools needed
    model: kimiModel,

    buildPrompt(state: GraphState): string {
        const parts: string[] = [];

        // If we have the original image as base64, include it
        if (state.artifacts["input_image"]) {
            parts.push(
                "Here is the screenshot of the UI to build:\n\n" +
                `[Image provided as base64 in artifacts — data:image/png;base64,${state.artifacts["input_image"].slice(0, 100)}...]`
            );
        }

        parts.push("User request: " + state.input);
        return parts.join("\n\n");
    },

    parseResult(output: string, _state: GraphState): NodeResult {
        return {
            output,
            artifacts: { plan: output },
        };
    },
};
