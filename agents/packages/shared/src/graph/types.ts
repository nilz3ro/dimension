import type { AgentTool } from "@mariozechner/pi-agent-core";
import type { Model } from "@mariozechner/pi-ai";

/** Accumulated state passed between graph nodes. */
export interface GraphState {
    /** The original user input / task description. */
    input: string;
    /** Messages accumulated across node executions (text summaries from each node). */
    messages: NodeMessage[];
    /** Named artifacts produced by nodes (screenshots, code, plans, etc.). */
    artifacts: Record<string, string>;
    /** Current iteration count (incremented each time a node runs). */
    iteration: number;
    /** Whether the graph should stop executing. */
    done: boolean;
}

export interface NodeMessage {
    node: string;
    content: string;
}

/** Result returned by a node after execution. */
export interface NodeResult {
    /** Text output from the node's agent. */
    output: string;
    /** Artifacts to merge into graph state (key → value). */
    artifacts?: Record<string, string>;
    /** If true, signals the graph to stop. */
    done?: boolean;
}

/** Condition function that determines whether an edge should be followed. */
export type EdgeCondition = (state: GraphState) => boolean;

/** Configuration for a graph node. */
export interface NodeConfig {
    /** Unique node identifier. */
    id: string;
    /** System prompt for the node's agent. */
    systemPrompt: string;
    /** Tools available to the node's agent. */
    tools: AgentTool[];
    /** Model to use (defaults to graph-level default). */
    model?: Model<any>;
    /** Builds the user message from current graph state. */
    buildPrompt: (state: GraphState) => string;
    /** Extracts structured results from the agent's raw text output. */
    parseResult: (output: string, state: GraphState) => NodeResult;
}

/** Configuration for a graph edge. */
export interface EdgeConfig {
    from: string;
    to: string;
    /** If provided, edge is only followed when condition returns true. */
    condition?: EdgeCondition;
}

/** Options for the graph runner. */
export interface GraphRunOptions {
    /** Maximum total node executions before forced stop. Default: 20. */
    maxIterations?: number;
    /** Called after each node execution. */
    onNodeComplete?: (nodeId: string, result: NodeResult, state: GraphState) => void;
}
