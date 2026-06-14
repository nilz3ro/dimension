import { Agent } from "@mariozechner/pi-agent-core";
import type { Model } from "@mariozechner/pi-ai";
import type { NodeConfig, NodeResult, GraphState } from "./types.js";

/**
 * Wraps a NodeConfig and runs it as an agent, returning structured results.
 */
export class AgentNode {
    readonly id: string;
    private readonly config: NodeConfig;

    constructor(config: NodeConfig) {
        this.id = config.id;
        this.config = config;
    }

    /**
     * Execute this node's agent against the current graph state.
     */
    async run(state: GraphState, defaultModel: Model<any>): Promise<NodeResult> {
        const model = this.config.model ?? defaultModel;
        const userMessage = this.config.buildPrompt(state);

        const agent = new Agent({
            initialState: {
                model,
                systemPrompt: this.config.systemPrompt,
                tools: this.config.tools,
            },
        });

        const output = await new Promise<string>((resolve) => {
            agent.subscribe((e) => {
                if (e.type === "agent_end") {
                    const lastAssistant = [...e.messages]
                        .reverse()
                        .find((m: { role: string }) => m.role === "assistant");

                    if (lastAssistant) {
                        const content = lastAssistant.content;
                        if (Array.isArray(content)) {
                            const text = content
                                .filter((block: { type: string }) => block.type === "text")
                                .map((block: { type: string; text?: string }) => block.text ?? "")
                                .join("");
                            resolve(text);
                        } else if (typeof content === "string") {
                            resolve(content);
                        } else {
                            resolve("");
                        }
                    } else {
                        resolve("");
                    }
                }
            });

            agent.prompt(userMessage);
        });

        return this.config.parseResult(output, state);
    }
}
