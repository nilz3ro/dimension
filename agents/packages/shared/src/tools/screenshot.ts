import fs from "node:fs/promises";
import path from "node:path";
import os from "node:os";
import { spawn } from "node:child_process";
import { Type } from "@sinclair/typebox";
import { Value } from "@sinclair/typebox/value";
import type { AgentTool } from "@mariozechner/pi-agent-core";

const ScreenshotInput = Type.Object({
    html: Type.String({ description: "HTML content to render and screenshot" }),
    width: Type.Optional(Type.Number({ description: "Viewport width in pixels (default: 1280)" })),
    height: Type.Optional(Type.Number({ description: "Viewport height in pixels (default: 800)" })),
});

/**
 * Takes a screenshot of HTML content using Puppeteer in headless Chromium.
 * Returns the screenshot as a base64-encoded PNG string.
 */
export const screenshotTool: AgentTool = {
    name: "screenshot",
    label: "Screenshot",
    description: `Render HTML content in a headless browser and take a screenshot.
Write the HTML you want to screenshot, and this tool will:
1. Save it to a temp file
2. Open it in headless Chromium via Puppeteer
3. Take a full-page screenshot
4. Return the screenshot as base64 PNG

Use this to preview what your HTML/CSS/JS output looks like.`,
    parameters: ScreenshotInput,
    execute: async (_toolCallId, params, _signal, _onUpdate) => {
        const p = Value.Parse(ScreenshotInput, params);
        const width = p.width ?? 1280;
        const height = p.height ?? 800;

        // Write HTML to a temp file
        const tmpDir = await fs.mkdtemp(path.join(os.tmpdir(), "screenshot-"));
        const htmlPath = path.join(tmpDir, "page.html");
        const screenshotPath = path.join(tmpDir, "screenshot.png");
        await fs.writeFile(htmlPath, p.html, "utf-8");

        // Puppeteer script to run in a subprocess
        const puppeteerScript = `
const puppeteer = require('puppeteer');
(async () => {
    const browser = await puppeteer.launch({
        headless: 'new',
        args: ['--no-sandbox', '--disable-setuid-sandbox', '--disable-dev-shm-usage'],
    });
    const page = await browser.newPage();
    await page.setViewport({ width: ${width}, height: ${height} });
    await page.goto('file://${htmlPath}', { waitUntil: 'networkidle0', timeout: 15000 });
    await page.screenshot({ path: '${screenshotPath}', fullPage: true });
    await browser.close();
})();
`;

        try {
            // Run puppeteer in a child process
            await new Promise<void>((resolve, reject) => {
                const child = spawn("node", ["-e", puppeteerScript], {
                    stdio: ["pipe", "pipe", "pipe"],
                    timeout: 30000,
                });

                const stderr: string[] = [];
                child.stderr.on("data", (data: Buffer) => stderr.push(data.toString()));

                child.on("close", (code) => {
                    if (code === 0) {
                        resolve();
                    } else {
                        reject(new Error(`Puppeteer exited with code ${code}: ${stderr.join("")}`));
                    }
                });

                child.on("error", reject);
                child.stdin.end();
            });

            // Read screenshot as base64
            const screenshotBuffer = await fs.readFile(screenshotPath);
            const base64 = screenshotBuffer.toString("base64");

            // Cleanup
            await fs.rm(tmpDir, { recursive: true, force: true });

            return {
                content: [{ type: "text", text: base64 }],
                details: { width, height, size: screenshotBuffer.length },
            };
        } catch (err) {
            // Cleanup on error
            await fs.rm(tmpDir, { recursive: true, force: true }).catch(() => {});

            const message = err instanceof Error ? err.message : String(err);
            return {
                content: [{ type: "text", text: `Screenshot failed: ${message}` }],
                details: { error: true },
            };
        }
    },
};
