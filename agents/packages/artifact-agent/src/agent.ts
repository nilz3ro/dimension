import fs from "node:fs";
import { runAgent, readFileTool, writeFileTool, artifactUploadTool, artifactDownloadTool, artifactListTool } from "@dimension-agents/shared";

const prompt = fs.readFileSync(new URL("../src/prompts/artifact-agent.md", import.meta.url), "utf-8");

runAgent({
    systemPrompt: prompt,
    tools: [
        readFileTool,
        writeFileTool,
        artifactUploadTool,
        artifactDownloadTool,
        artifactListTool,
    ],
}).catch((err: unknown) => {
    process.stderr.write(String(err) + "\n");
    process.exit(1);
});
