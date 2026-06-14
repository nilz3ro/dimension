import { Agent } from "@mariozechner/pi-agent-core";
import type { AgentTool } from "@mariozechner/pi-agent-core";
import type { Message, Model } from "@mariozechner/pi-ai";
import { createCompatModel } from "./model.js";

interface DimensionContent {
    type: string;
    text?: string;
}

interface HistoryEntry {
    role: string;
    content: string;
    timestamp: string;
}

interface DimensionRequest {
    role: string;
    content: DimensionContent[];
    session_id: string;
    bundle_id: string;
    history: HistoryEntry[];
    truncation: {
        total_messages: number;
        included_messages: number;
        truncated: boolean;
    };
}

/**
 * Build the default model for Dimension agents.
 * Uses vLLM served on the AI workbox when VLLM_BASE_URL is set,
 * otherwise falls back to OpenAI. createCompatModel (rather than
 * pi-ai's getModel registry) so arbitrary model names work via
 * the OPENAI_MODEL / VLLM_MODEL env vars.
 */
function getDefaultModel(): Model<any> {
    const vllmBase = process.env.VLLM_BASE_URL;
    if (vllmBase) {
        const vllmModel = process.env.VLLM_MODEL || "openai/gpt-oss-20b";
        return createCompatModel({
            id: vllmModel,
            name: vllmModel,
            baseUrl: vllmBase,
            provider: "vllm",
            apiKeyEnvVariable: "VLLM_API_KEY",
            reasoning: true,
            contextWindow: 131072,
            maxTokens: 16384,
        });
    }
    const openaiModel = process.env.OPENAI_MODEL || "gpt-4.1";
    return createCompatModel({
        id: openaiModel,
        name: openaiModel,
        baseUrl: "https://api.openai.com/v1",
        provider: "openai",
        apiKeyEnvVariable: "OPENAI_API_KEY",
        reasoning: false,
        contextWindow: 131072,
        maxTokens: 16384,
    });
}

export interface AgentConfig {
    systemPrompt: string;
    tools: AgentTool[];
    /** Pre-built model object. Defaults to vLLM (if VLLM_BASE_URL set) or OpenAI gpt-4.1 */
    model?: Model<any>;
}

function readStdin(): Promise<string> {
    return new Promise((resolve, reject) => {
        const chunks: Buffer[] = [];
        process.stdin.on("data", (chunk: Buffer) => chunks.push(chunk));
        process.stdin.on("end", () => resolve(Buffer.concat(chunks).toString("utf-8")));
        process.stdin.on("error", reject);
    });
}

function historyToMessages(history: HistoryEntry[]): Message[] {
    return history.map(entry => {
        const ts = Date.parse(entry.timestamp) || Date.now();
        if (entry.role === "user") {
            return {
                role: "user" as const,
                content: [{ type: "text" as const, text: entry.content }],
                timestamp: ts,
            };
        } else {
            return {
                role: "assistant" as const,
                content: [{ type: "text" as const, text: entry.content }],
                api: "openai-responses" as const,
                provider: "openai",
                model: "gpt-4.1",
                usage: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, totalTokens: 0, cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 } },
                stopReason: "stop" as const,
                timestamp: ts,
            };
        }
    });
}

// ── Agent lifecycle ───────────────────────────────────────────────────────────

/**
 * Parse a Dimension request from stdin and create an Agent instance.
 */
export function createAgent(config: AgentConfig): { agent: Agent; userMessage: string; historyMessages: Message[] } {
    const model = config.model ?? getDefaultModel();

    const agent = new Agent({
        initialState: {
            model,
            systemPrompt: config.systemPrompt,
            tools: config.tools,
        },
    });

    return { agent, userMessage: "", historyMessages: [] };
}

/**
 * Full agent lifecycle: read stdin → parse request → run agent → write
 * final response to stdout.
 *
 * Bundles that want to emit progress events during the run should import
 * `dimension.send(...)` from `./events.js` and call it directly. The
 * agent itself never auto-emits — bundle code controls what flows into
 * the outbound message stream.
 */
export async function runAgent(config: AgentConfig): Promise<void> {
    const raw = (await readStdin()).trim();
    if (!raw) process.exit(1);

    let userMessage: string;
    let historyMessages: Message[] = [];
    let sessionId = "";

    try {
        const req: DimensionRequest = JSON.parse(raw);
        userMessage = req.content
            .filter((c: DimensionContent) => c.type === "text")
            .map((c: DimensionContent) => c.text ?? "")
            .join("");

        sessionId = req.session_id ?? "";

        if (sessionId) {
            process.env["CONVERSATION_ID"] = sessionId;
        }

        if (req.history && req.history.length > 0) {
            historyMessages = historyToMessages(req.history);
        }
    } catch {
        userMessage = raw;
    }

    if (!userMessage) {
        console.log("No user message found. exiting.");
        process.exit(1);
    }

    const model = config.model ?? getDefaultModel();

    const agent = new Agent({
        initialState: {
            model,
            systemPrompt: config.systemPrompt,
            tools: config.tools,
            messages: historyMessages,
        },
    });

    await new Promise<void>((resolve, _reject) => {
        agent.subscribe((e) => {
            if (e.type === "agent_end") {
                const lastAssistant = [...e.messages]
                    .reverse()
                    .find((m: { role: string }) => m.role === "assistant");
                if (lastAssistant) {
                    const content = lastAssistant.content;
                    let text = "";
                    if (Array.isArray(content)) {
                        text = content
                            .filter((block: { type: string }) => block.type === "text")
                            .map((block: { type: string; text?: string }) => block.text ?? "")
                            .join("");
                    } else if (typeof content === "string") {
                        text = content;
                    }
                    process.stdout.write(text);
                }

                resolve();
            }
        });

        agent.prompt(userMessage);
    });

    // Brief delay so the session-store save and any pending outbound
    // events can drain before the process exits.
    await new Promise((r) => setTimeout(r, 500));

    process.exit(0);
}
