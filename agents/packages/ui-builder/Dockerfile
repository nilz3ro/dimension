FROM node:22-slim

# Install Chromium and dependencies for Puppeteer
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
       chromium \
       fonts-liberation \
       fonts-noto-color-emoji \
       libgbm1 \
       libnss3 \
       libatk-bridge2.0-0 \
       libatk1.0-0 \
       libcups2 \
       libdrm2 \
       libxcomposite1 \
       libxdamage1 \
       libxrandr2 \
       libxss1 \
       libxtst6 \
       grep \
       curl \
    && rm -rf /var/lib/apt/lists/*

# Tell Puppeteer to use installed Chromium instead of downloading its own
ENV PUPPETEER_SKIP_CHROMIUM_DOWNLOAD=true
ENV PUPPETEER_EXECUTABLE_PATH=/usr/bin/chromium

RUN printf "nameserver 8.8.8.8\nnameserver 1.1.1.1\n" > /etc/resolv.conf.vm

RUN corepack enable && corepack prepare pnpm@latest --activate

WORKDIR /app

# Copy workspace root
COPY package.json pnpm-workspace.yaml pnpm-lock.yaml ./
COPY packages/shared/package.json packages/shared/
COPY packages/ui-builder/package.json packages/ui-builder/

RUN pnpm install --frozen-lockfile

COPY packages/shared/ packages/shared/
COPY packages/ui-builder/ packages/ui-builder/

RUN pnpm -r build

COPY packages/ui-builder/entrypoint.sh /app/entrypoint.sh
COPY packages/ui-builder/dimension.toml /etc/hyphae/dimension.toml
RUN chmod +x /app/entrypoint.sh

RUN mkdir -p /app/workspace
ENV WORKSPACE_DIR=/app/workspace
ENV VLLM_BASE_URL=http://100.104.71.102:8000
ENV VLLM_MODEL=openai/gpt-oss-20b
ENV VLLM_API_KEY=none
ENV MOONSHOT_API_KEY=placeholder

ENTRYPOINT ["/app/entrypoint.sh"]
