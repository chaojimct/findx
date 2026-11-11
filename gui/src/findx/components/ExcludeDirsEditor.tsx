import { useState, useEffect } from "react";
import "./exclude-dirs-editor.css";

interface ExcludeDirsEditorProps {
  value: string[];
  onChange: (dirs: string[]) => void;
  hostOs: string;
}

export function ExcludeDirsEditor({ value, onChange, hostOs }: ExcludeDirsEditorProps) {
  const [text, setText] = useState(value.join("\n"));

  useEffect(() => {
    setText(value.join("\n"));
  }, [value]);

  const handleChange = (newText: string) => {
    setText(newText);
    const dirs = newText
      .split(/\r?\n/)
      .map((x) => x.trim())
      .filter(Boolean);
    onChange(dirs);
  };

  const handleClear = () => {
    if (value.length === 0) return;
    
    if (
      !confirm(
        `确定要清空全部 ${value.length} 条排除规则吗？\n\n` +
          "清空后需要「重建索引」才能扫描之前被排除的目录。\n" +
          "这是一个危险操作，请确认。"
      )
    ) {
      return;
    }
    
    handleChange("");
  };

  const exampleDirs = hostOs === "windows"
    ? ["C:\\Windows\\WinSxS", "C:\\Program Files (x86)\\Common Files", "C:\\$Recycle.Bin"]
    : hostOs === "darwin" || hostOs === "macos"
      ? ["/System/Volumes/Data/private/var/folders", "/Library/Caches", "~/Library/Logs"]
      : ["/usr/share/doc", "/var/log", "/tmp"];

  const handleAddExamples = () => {
    const existing = new Set(value);
    const newDirs = exampleDirs.filter((dir) => !existing.has(dir));
    
    if (newDirs.length === 0) {
      alert("示例目录已全部添加");
      return;
    }
    
    handleChange([...value, ...newDirs].join("\n"));
  };

  const lineCount = value.length;

  return (
    <div className="fx-exclude-dirs-editor">
      <div className="fx-exclude-dirs-header">
        <span className="fx-exclude-dirs-count">
          {lineCount === 0 ? "无排除规则" : `${lineCount} 条排除规则`}
        </span>
        <div className="fx-exclude-dirs-actions">
          <button
            type="button"
            className="fx-btn-link"
            onClick={handleAddExamples}
            title="添加常见排除目录示例"
          >
            添加示例
          </button>
          <button
            type="button"
            className="fx-btn-link fx-btn-danger"
            onClick={handleClear}
            disabled={lineCount === 0}
            title="清空所有排除规则（危险操作）"
          >
            全部清空
          </button>
        </div>
      </div>
      <textarea
        value={text}
        onChange={(e) => handleChange(e.target.value)}
        placeholder={`每行一个目录路径，例如：\n${exampleDirs[0]}`}
        className="fx-exclude-dirs-textarea"
      />
    </div>
  );
}
