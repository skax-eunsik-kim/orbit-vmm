# syntax=docker/dockerfile:1
# Exercises what real project builds lean on: a remote "# syntax" frontend,
# COPY --from a ghcr.io image, apt and uv behind BuildKit cache mounts.
FROM python:3.12-slim
COPY --from=ghcr.io/astral-sh/uv:0.8 /uv /usr/local/bin/uv
RUN rm -f /etc/apt/apt.conf.d/docker-clean
RUN --mount=type=cache,target=/var/cache/apt,sharing=locked \
    --mount=type=cache,target=/var/lib/apt,sharing=locked \
    apt-get update && apt-get install -y --no-install-recommends libpq5
WORKDIR /app
COPY requirements.txt .
RUN --mount=type=cache,target=/root/.cache/uv uv pip install --system -r requirements.txt
COPY alembic.ini .
COPY migrations migrations
CMD ["alembic", "upgrade", "head"]
