import fs from "node:fs";
import { createCompatModel } from "@dimension-agents/shared";
import type { NodeConfig, NodeResult, GraphState } from "@dimension-agents/shared";

const systemPrompt = fs.readFileSync(
    new URL("../../src/prompts/reviewer.md", import.meta.url),
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
 * Reviewer node: compares the original screenshot with the built result using
 * Kimi k2.5 vision. Decides whether to approve or request revisions.
 */
export const reviewerNode: NodeConfig = {
    id: "reviewer",
    systemPrompt,
    tools: [], // Reviewer only compares and outputs verdict — no tools needed
    model: kimiModel,

    buildPrompt(state: GraphState): string {
        const parts: string[] = [];

        // Original target
        if (state.artifacts["input_image"]) {
            parts.push("## Original Target Screenshot\n\n[base64 image provided in input_image artifact]");
        } else {
            parts.push("## Original Request\n\n" + state.input);
        }

        // Plan used
        if (state.artifacts["plan"]) {
            parts.push("## Plan Used\n\n" + state.artifacts["plan"]);
        }

        // Built screenshot
        if (state.artifacts["built_screenshot"]) {
            parts.push(
                "## Built Result Screenshot\n\n" +
                "[base64 screenshot of what was built, provided in built_screenshot artifact]"
            );
        }

        // How many review rounds so far
        const reviewCount = state.messages.filter(m => m.node === "reviewer").length;
        if (reviewCount >= 2) {
            parts.push(
                `Note: This is review round ${reviewCount + 1}. Be more lenient — ` +
                "if the result is reasonably close to the target, approve it."
            );
        }

        parts.push("Compare the original target with the built result and provide your verdict.");

        return parts.join("\n\n");
    },

    parseResult(output: string, _state: GraphState): NodeResult {
        const approved = output.includes("STATUS: APPROVED");

        return {
            output,
            done: approved,
            artifacts: { review_verdict: approved ? "approved" : "needs_revision" },
        };
    },
};
