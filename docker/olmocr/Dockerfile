# Official linux/amd64 image, pinned 2026-09-02 by OCI index digest.
FROM docker.io/alleninstituteforai/olmocr:latest-with-model@sha256:ed24649234d83730129ecaaa60736843f12a4937d0be9a28ebe3f44b8fb2e735

COPY --chmod=0555 run-olmocr /usr/local/bin/run-olmocr

ENV HF_HOME=/opt/models \
    HF_HUB_OFFLINE=1 \
    TRANSFORMERS_OFFLINE=1 \
    OLMOCR_MODEL=allenai/olmOCR-2-7B-1025-FP8 \
    HOME=/tmp/home \
    XDG_CACHE_HOME=/tmp/cache

USER 65532:65532
ENTRYPOINT ["/usr/local/bin/run-olmocr"]
