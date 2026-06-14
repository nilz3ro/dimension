import { AgentGraph, sendMessage } from "@dimension-agents/shared";
import type { GraphState, NodeResult } from "@dimension-agents/shared";

import { plannerNode } from "./nodes/planner.js";
import { builderNode } from "./nodes/builder.js";
import { reviewerNode } from "./nodes/reviewer.js";

/**
 * UI Builder agent — a 3-node graph loop that builds UIs from screenshots.
 *
 * Flow: Planner → Builder → Reviewer → (if not approved) Builder → Reviewer → ...
 *
 * - Planner: Analyzes the input screenshot with Kimi k2.5 vision, produces a plan
 * - Builder: Writes HTML/CSS/JS code and takes a screenshot of the result
 * - Reviewer: Compares original vs built screenshot, approves or sends feedback
 *
 * Standalone: reads stdin JSON, writes response to stdout.
 */

function readStdin(): Promise<string> {
    return new Promise((resolve, reject) => {
        const chunks: Buffer[] = [];
        process.stdin.on("data", (chunk: Buffer) => chunks.push(chunk));
        process.stdin.on("end", () => resolve(Buffer.concat(chunks).toString("utf-8")));
        process.stdin.on("error", reject);
    });
}

interface DimensionContent {
    type: string;
    text?: string;
}

interface DimensionRequest {
    role: string;
    content: DimensionContent[];
    session_id?: string;
}

async function main(): Promise<void> {
    const raw = (await readStdin()).trim();
    if (!raw) process.exit(1);

    // Parse input (Dimension request JSON or plain text)
    let userMessage: string;
    let sessionId = "";
    try {
        const req: DimensionRequest = JSON.parse(raw);
        userMessage = req.content
            .filter((c) => c.type === "text")
            .map((c) => c.text ?? "")
            .join("");

        sessionId = req.session_id ?? "";

        if (sessionId) {
            process.env["CONVERSATION_ID"] = sessionId;
        }
    } catch {
        userMessage = raw;
    }

    if (!userMessage) process.exit(1);

    // Build the agent graph
    const graph = new AgentGraph();

    graph.addNode(plannerNode);
    graph.addNode(builderNode);
    graph.addNode(reviewerNode);

    // Planner always flows to Builder
    graph.addEdge({ from: "planner", to: "builder" });

    // Builder always flows to Reviewer
    graph.addEdge({ from: "builder", to: "reviewer" });

    // Reviewer flows back to Builder if not done (the graph stops when done=true)
    graph.addEdge({
        from: "reviewer",
        to: "builder",
        condition: (state: GraphState) => !state.done,
    });

    // Run the graph
    const initialState: Partial<GraphState> = {
        input: userMessage,
    };

    // Check if user provided a base64 image in their message
    // Convention: lines starting with "IMAGE:" contain base64 image data
    const imageMatch = userMessage.match(/^IMAGE:(.+)$/m);
    if (imageMatch) {
        initialState.artifacts = { input_image: imageMatch[1].trim() };
        initialState.input = userMessage.replace(/^IMAGE:.+$/m, "").trim();
    }

    const result = await graph.run("planner", initialState, {
        maxIterations: 10,
        onNodeComplete: (nodeId: string, nodeResult: NodeResult, state: GraphState) => {
            process.stderr.write(
                `[ui-builder] Node "${nodeId}" completed (iteration ${state.iteration}, done=${state.done})\n`
            );
            if (nodeId === "reviewer" && nodeResult.done) {
                process.stderr.write("[ui-builder] Reviewer approved! Build complete.\n");
            }
        },
    });

    // Output the final result
    const lastMessage = result.messages[result.messages.length - 1];
    let msg: string;
    if (result.done) {
        msg =
            `UI build completed successfully after ${result.iteration} iterations.\n` +
            `Output file: /app/workspace/ui-output.html\n\n` +
            `Final review:\n${lastMessage?.content ?? "(no output)"}`;
    } else {
        msg =
            `UI build stopped after ${result.iteration} iterations (max reached).\n` +
            `Output file: /app/workspace/ui-output.html\n\n` +
            `Last output:\n${lastMessage?.content ?? "(no output)"}`;
    }

    process.stdout.write(msg);
    await sendMessage(msg, { sessionId });

    process.exit(0);
}

main().catch((err: unknown) => {
    process.stderr.write(String(err) + "\n");
    process.exit(1);
});
