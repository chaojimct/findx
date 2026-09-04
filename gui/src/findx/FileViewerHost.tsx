import ofdRenderer from "@file-viewer/renderer-ofd";
import pdfRenderer from "@file-viewer/renderer-pdf";
import { pptxRenderer } from "@file-viewer/renderer-presentation/pptx";
import spreadsheetRenderer from "@file-viewer/renderer-spreadsheet";
import wordRenderer from "@file-viewer/renderer-word";
import { FileViewer, type ViewerEvent, type ViewerOptions, type ViewerState } from "@file-viewer/react";
import { convertFileSrc } from "@tauri-apps/api/core";
import { useEffect, useMemo, useRef, useState } from "react";
import { VIEWER_MAX_BYTES, formatViewerError, formatViewerSize } from "./viewerPreviewSupport";

export type FileViewerHostProps = {
  path: string;
  onError: (message: string) => void;
  onReady: () => void;
};

function currentFxTheme(): "light" | "dark" {
  return document.documentElement.getAttribute("data-fx-theme") === "dark" ? "dark" : "light";
}

async function loadLocalFile(path: string, signal: AbortSignal): Promise<File> {
  const url = convertFileSrc(path);
  const res = await fetch(url, { signal });
  if (!res.ok) {
    throw new Error(`无法读取本地文件（HTTP ${res.status}）`);
  }
  const blob = await res.blob();
  const name = path.split(/[\\/]/).pop() || "file";
  return new File([blob], name, { type: blob.type || "application/octet-stream" });
}

export default function FileViewerHost({ path, onError, onReady }: FileViewerHostProps) {
  const [file, setFile] = useState<File | null>(null);
  const [theme, setTheme] = useState<"light" | "dark">(currentFxTheme);
  const onErrorRef = useRef(onError);
  const onReadyRef = useRef(onReady);
  onErrorRef.current = onError;
  onReadyRef.current = onReady;

  useEffect(() => {
    const el = document.documentElement;
    const obs = new MutationObserver(() => setTheme(currentFxTheme()));
    obs.observe(el, { attributes: true, attributeFilter: ["data-fx-theme"] });
    return () => obs.disconnect();
  }, []);

  useEffect(() => {
    const ac = new AbortController();
    setFile(null);
    void (async () => {
      try {
        const file = await loadLocalFile(path, ac.signal);
        if (file.size > VIEWER_MAX_BYTES) {
          throw new Error(`文件过大（${formatViewerSize(file.size)}），内置预览暂不支持超过 80 MB 的文档`);
        }
        if (!ac.signal.aborted) setFile(file);
      } catch (err) {
        if (ac.signal.aborted) return;
        onErrorRef.current(formatViewerError(err));
      }
    })();
    return () => ac.abort();
  }, [path]);

  const options = useMemo(
    () => ({
      renderers: [pdfRenderer, wordRenderer, spreadsheetRenderer, pptxRenderer, ofdRenderer] as unknown as ViewerOptions["renderers"],
      rendererMode: "replace" as const,
      theme,
      styleIsolation: "shadow" as const,
      ui: { density: "compact" as const },
      toolbar: {
        position: "bottom-right" as const,
        download: false,
        print: false,
        exportHtml: false,
        zoom: true,
      },
      search: { enabled: true },
      pdf: {
        toolbar: false,
        navigation: false,
        defaultNavigationVisible: false,
      },
    }),
    [theme],
  );

  if (!file) return null;

  return (
    <FileViewer
      key={path}
      className="fx-preview-file-viewer"
      file={file}
      filename={file.name}
      options={options}
      onStateChange={(state: ViewerState) => {
        if (state.error) {
          onErrorRef.current(formatViewerError(state.error));
          return;
        }
        if (state.ready) onReadyRef.current();
      }}
      onEvent={(event: ViewerEvent) => {
        if (event.type === "load-complete") onReadyRef.current();
      }}
    />
  );
}
