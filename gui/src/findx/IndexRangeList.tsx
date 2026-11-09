import { partitionIndexRanges, toggleRangeValue } from "./indexRange";

/**
 * 范围列表：名称 + 完整路径。系统挂载默认折叠。
 * onChange 得到的仍是原始路径字符串。
 */
export function IndexRangeList({
  paths,
  selected,
  onChange,
}: {
  paths: readonly string[];
  selected: readonly string[];
  onChange: (next: string[]) => void;
}) {
  const { general, system } = partitionIndexRanges(paths);
  const selectedSystem = system.filter((item) => selected.includes(item.value)).length;
  const renderItem = (item: { value: string; title: string; detail: string }) => {
    const checked = selected.includes(item.value);
    return (
      <label key={item.value} className="fx-range-item">
        <input
          type="checkbox"
          checked={checked}
          onChange={(e) => onChange(toggleRangeValue(selected, item.value, e.target.checked))}
        />
        <span className="fx-range-title">{item.title}</span>
        <span className="fx-range-detail">{item.detail}</span>
      </label>
    );
  };
  return (
    <div className="fx-range-list">
      {general.length > 0 && <div className="fx-range-group">{general.map(renderItem)}</div>}
      {system.length > 0 && (
        <details className="fx-range-system">
          <summary>
            系统挂载 {system.length} 项
            {selectedSystem > 0 ? `，已选 ${selectedSystem}` : ""}
          </summary>
          <div className="fx-range-group">{system.map(renderItem)}</div>
        </details>
      )}
    </div>
  );
}
