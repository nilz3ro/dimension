import type { Model } from "@mariozechner/pi-ai";
import { getModel } from "@mariozechner/pi-ai";
import { AgentNode } from "./node.js";
import type { NodeConfig, EdgeConfig, GraphState, GraphRunOptions } from "./types.js";

/**
 * A directed graph of agent nodes with conditional edges and loop support.
 *
 * Usage:
 *   const graph = new AgentGraph();
 *   graph.addNode({ id: "planner", ... });
 *   graph.addNode({ id: "builder", ... });
 *   graph.addEdge({ from: "planner", to: "builder" });
 *   const result = await graph.run("planner", { input: "build a login page" });
 */
export class AgentGraph {
    private nodes = new Map<string, AgentNode>();
    private edges: EdgeConfig[] = [];
    private defaultModel: Model<any>;

    constructor(defaultModel?: Model<any>) {
        this.defaultModel = defaultModel ?? getModel("openai", "gpt-4.1");
    }

    /** Register a node in the graph. */
    addNode(config: NodeConfig): this {
        this.nodes.set(config.id, new AgentNode(config));
        return this;
    }

    /** Add a directed edge between two nodes, optionally with a condition. */
    addEdge(edge: EdgeConfig): this {
        if (!this.nodes.has(edge.from)) {
            throw new Error(`Edge source node "${edge.from}" not found`);
        }
        if (!this.nodes.has(edge.to)) {
            throw new Error(`Edge target node "${edge.to}" not found`);
        }
        this.edges.push(edge);
        return this;
    }

    /**
     * Run the graph starting from the given entry node.
     * Returns the final graph state when done or maxIterations is reached.
     */
    async run(entryNodeId: string, initialState: Partial<GraphState>, options?: GraphRunOptions): Promise<GraphState> {
        const maxIterations = options?.maxIterations ?? 20;

        if (!this.nodes.has(entryNodeId)) {
            throw new Error(`Entry node "${entryNodeId}" not found`);
        }

        const state: GraphState = {
            input: initialState.input ?? "",
            messages: initialState.messages ?? [],
            artifacts: initialState.artifacts ?? {},
            iteration: 0,
            done: false,
        };

        let currentNodeId: string | null = entryNodeId;

        while (currentNodeId && !state.done && state.iteration < maxIterations) {
            const node = this.nodes.get(currentNodeId);
            if (!node) {
                throw new Error(`Node "${currentNodeId}" not found during execution`);
            }

            state.iteration++;
            process.stderr.write(`[graph] Running node "${currentNodeId}" (iteration ${state.iteration}/${maxIterations})\n`);

            const result = await node.run(state, this.defaultModel);

            // Update state with node results
            state.messages.push({ node: currentNodeId, content: result.output });
            if (result.artifacts) {
                Object.assign(state.artifacts, result.artifacts);
            }
            if (result.done) {
                state.done = true;
            }

            options?.onNodeComplete?.(currentNodeId, result, state);

            if (state.done) break;

            // Find next node via edges
            currentNodeId = this.resolveNextNode(currentNodeId, state);
        }

        if (state.iteration >= maxIterations && !state.done) {
            process.stderr.write(`[graph] Max iterations (${maxIterations}) reached, stopping.\n`);
        }

        return state;
    }

    /**
     * Evaluate outgoing edges from the current node and return the first matching target.
     * Unconditional edges (no condition) always match.
     * Conditional edges are evaluated in order; first true wins.
     */
    private resolveNextNode(fromId: string, state: GraphState): string | null {
        const outgoing = this.edges.filter(e => e.from === fromId);
        if (outgoing.length === 0) return null;

        // Conditional edges first, then unconditional as fallback
        const conditional = outgoing.filter(e => e.condition);
        for (const edge of conditional) {
            if (edge.condition!(state)) {
                return edge.to;
            }
        }

        // Fall back to unconditional edge
        const unconditional = outgoing.find(e => !e.condition);
        return unconditional?.to ?? null;
    }
}
