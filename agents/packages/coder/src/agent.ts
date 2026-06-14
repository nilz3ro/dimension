import fs from "node:fs";
import { runAgent, readFileTool, writeFileTool, editFileTool, listDirTool, bashTool, grepTool, webSearchTool, webFetchTool, a2aSendTool, a2aDiscoverTool, artifactUploadTool, artifactDownloadTool, artifactListTool, deployTool } from "@dimension-agents/shared";

// Prompt file lives in src/ (not copied to dist/ by tsc)
const prompt = fs.readFileSync(new URL("../src/prompts/coder.md", import.meta.url), "utf-8");

runAgent({
    systemPrompt: prompt,
    tools: [
        readFileTool,
        writeFileTool,
        editFileTool,
        listDirTool,
        bashTool,
        grepTool,
        webSearchTool,
        webFetchTool,
        a2aSendTool,
        a2aDiscoverTool,
        artifactUploadTool,
        artifactDownloadTool,
        artifactListTool,
        deployTool,
    ],
}).catch((err: unknown) => {
    process.stderr.write(String(err) + "\n");
    process.exit(1);
});
