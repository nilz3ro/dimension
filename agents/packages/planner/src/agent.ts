import fs from "node:fs";
import { runAgent, readFileTool, writeFileTool, editFileTool, listDirTool, grepTool, webSearchTool, webFetchTool, gsdTool, a2aSendTool, a2aDiscoverTool } from "@dimension-agents/shared";

const prompt = fs.readFileSync(new URL("../src/prompts/planner.md", import.meta.url), "utf-8");

runAgent({
    systemPrompt: prompt,
    tools: [
        readFileTool,
        writeFileTool,
        editFileTool,
        listDirTool,
        grepTool,
        webSearchTool,
        webFetchTool,
        gsdTool,
        a2aSendTool,
        a2aDiscoverTool,
    ],
}).catch((err: unknown) => {
    process.stderr.write(String(err) + "\n");
    process.exit(1);
});
