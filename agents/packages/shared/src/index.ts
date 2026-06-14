// Tools
export { readFileTool, writeFileTool, editFileTool, listDirTool } from "./tools/filesystem.js";
export { bashTool } from "./tools/shell.js";
export { grepTool } from "./tools/search.js";
export { webSearchTool, webFetchTool } from "./tools/web.js";
export { a2aSendTool, a2aDiscoverTool } from "./tools/a2a.js";
export { artifactUploadTool, artifactDownloadTool, artifactListTool } from "./tools/artifacts.js";
export { deployTool } from "./tools/deploy.js";
export { gsdTool } from "./tools/gsd.js";
export { screenshotTool } from "./tools/screenshot.js";

// Agent harness
export { createAgent, runAgent } from "./agent.js";
export type { AgentConfig } from "./agent.js";

// Outbound messaging
export { send, sendMessage, isConnected, dimension } from "./events.js";
export type { DimensionEvent } from "./events.js";

// Re-export model utilities so downstream packages don't need pi-ai directly
export { getModel } from "@mariozechner/pi-ai";
export type { Model, Api } from "@mariozechner/pi-ai";

// Custom model builder for OpenAI-compatible providers
export { createCompatModel } from "./model.js";

// Agent graph framework
export { AgentGraph, AgentNode } from "./graph/index.js";
export type { GraphState, NodeResult, EdgeCondition, NodeConfig, EdgeConfig, GraphRunOptions, NodeMessage } from "./graph/index.js";
