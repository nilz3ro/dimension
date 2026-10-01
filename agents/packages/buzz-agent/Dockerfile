FROM node:22-slim

RUN printf "nameserver 8.8.8.8\nnameserver 1.1.1.1\n" > /etc/resolv.conf.vm
RUN corepack enable && corepack prepare pnpm@10.7.0 --activate
# Shared depends on puppeteer, but this tool-free bundle needs no browser.
ENV PUPPETEER_SKIP_DOWNLOAD=true

WORKDIR /app
COPY package.json pnpm-workspace.yaml pnpm-lock.yaml ./
COPY packages/shared/package.json packages/shared/
COPY packages/buzz-agent/package.json packages/buzz-agent/
RUN pnpm install --frozen-lockfile

COPY packages/shared/ packages/shared/
COPY packages/buzz-agent/ packages/buzz-agent/
RUN pnpm -r build

COPY packages/buzz-agent/entrypoint.sh /app/entrypoint.sh
COPY packages/buzz-agent/dimension.toml /etc/hyphae/dimension.toml
RUN chmod +x /app/entrypoint.sh

ENTRYPOINT ["/app/entrypoint.sh"]
