import { execSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { Type } from "@sinclair/typebox";
import { Value } from "@sinclair/typebox/value";
import { AgentTool } from "@mariozechner/pi-agent-core";

const DeployInput = Type.Object({
    path: Type.String({ description: "Absolute path to the project directory to deploy" }),
    name: Type.Optional(Type.String({ description: "Name for the deployment artifact. Defaults to the directory name." })),
    build_command: Type.Optional(Type.String({ description: "Optional build command to run before packaging (e.g. 'npm run build')" })),
});

export const deployTool: AgentTool = {
    name: "deploy",
    label: "Deploy",
    description: "Package and publish a web app for deployment. Optionally runs a build command first, then tars the project directory and writes the artifact to /tmp. Returns the artifact path and size.",
    parameters: DeployInput,
    execute: async (_toolCallId, params, _signal, _onUpdate) => {
        const p = Value.Parse(DeployInput, params);

        try {
            // Validate path exists and is a directory
            const stat = fs.statSync(p.path);
            if (!stat.isDirectory()) {
                return {
                    content: [{ type: "text", text: `${p.path} is not a directory` }],
                    details: { error: true },
                };
            }

            const dirName = path.basename(p.path);
            const name = p.name ?? dirName;
            const tarName = `${name}.tar.gz`;
            const tarPath = `/tmp/${tarName}`;

            // Run build command if provided
            if (p.build_command) {
                try {
                    execSync(p.build_command, {
                        cwd: p.path,
                        stdio: ["pipe", "pipe", "pipe"],
                        timeout: 120_000,
                    });
                } catch (err) {
                    const msg = err instanceof Error ? err.message : String(err);
                    return {
                        content: [{ type: "text", text: `Build command failed: ${msg}` }],
                        details: { error: true },
                    };
                }
            }

            // Create tar.gz archive (exclude node_modules, .next/cache, .git)
            const parentDir = path.dirname(p.path);
            execSync(`tar czf ${tarPath} --exclude='node_modules' --exclude='.next/cache' --exclude='.git' -C ${parentDir} ${dirName}`, {
                stdio: ["pipe", "pipe", "pipe"],
                timeout: 60_000,
            });

            const data = fs.readFileSync(tarPath);
            const sizeMB = (data.length / (1024 * 1024)).toFixed(2);

            return {
                content: [{
                    type: "text",
                    text: `Packaged "${name}" at ${tarPath}\n  Size: ${data.length} bytes (${sizeMB} MB)`,
                }],
                details: { path: tarPath, size: data.length, name },
            };
        } catch (err) {
            return {
                content: [{ type: "text", text: `Deploy failed: ${err instanceof Error ? err.message : String(err)}` }],
                details: { error: true },
            };
        }
    },
};
