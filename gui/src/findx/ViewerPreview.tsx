import { lazy, Suspense } from "react";

const FileViewerHost = lazy(() => import("./FileViewerHost"));

export type ViewerPreviewProps = {
  path: string;
  onError: (message: string) => void;
  onReady: () => void;
};

/** 懒加载 Office 预览引擎，避免拖慢搜索主界面首屏。 */
export function ViewerPreview({ path, onError, onReady }: ViewerPreviewProps) {
  return (
    <Suspense fallback={null}>
      <FileViewerHost path={path} onError={onError} onReady={onReady} />
    </Suspense>
  );
}
