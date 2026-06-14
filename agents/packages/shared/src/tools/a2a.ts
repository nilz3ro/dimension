import { Type } from "@sinclair/typebox";
import { AgentTool } from "@mariozechner/pi-agent-core";

const UNAVAILABLE_MSG = "A2A inter-agent calls are not available in standalone mode. Agents run as isolated stdin/stdout processes.";

// ── A2A Send ─────────────────────────────────────────────────────────────────

const A2aSendInput = Type.Object({
    target: Type.String({ description: "Target agent bundle name (e.g. 'dimension-planner', 'dimension-coder'). Use a2a_discover first to list available agents." }),
    message: Type.String({ description: "Message to send to the agent" }),
});

export const a2aSendTool: AgentTool = {
    name: "a2a_send",
    label: "A2A Send",
    description: `Send a message to another agent on the platform and get a response.
Use for delegating tasks to specialized agents, asking other agents for information,
or collaborating across agent boundaries. Use a2a_discover first to see available agents.`,
    parameters: A2aSendInput,
    execute: async (_toolCallId, _params, _signal, _onUpdate) => {
        return {
            content: [{ type: "text", text: UNAVAILABLE_MSG }],
            details: { error: true },
        };
    },
};

// ── A2A Discover ─────────────────────────────────────────────────────────────

const A2aDiscoverInput = Type.Object({});

export const a2aDiscoverTool: AgentTool = {
    name: "a2a_discover",
    label: "A2A Discover",
    description: "List all available agents on the platform with their names and descriptions.",
    parameters: A2aDiscoverInput,
    execute: async (_toolCallId, _params, _signal, _onUpdate) => {
        return {
            content: [{ type: "text", text: UNAVAILABLE_MSG }],
            details: { error: true },
        };
    },
};
