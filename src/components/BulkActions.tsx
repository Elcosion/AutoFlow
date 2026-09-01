type BulkActionsProps = {
  count: number;
  total: number;
  onToggleAll: () => void;
  onDelete: () => void;
};

export function BulkActions({
  count,
  total,
  onToggleAll,
  onDelete,
}: BulkActionsProps) {
  return (
    <div className="bulk-actions">
      <label className="bulk-select-all">
        <input
          checked={total > 0 && count === total}
          disabled={total === 0}
          onChange={onToggleAll}
          type="checkbox"
        />
        <span>全选</span>
      </label>
      {count > 0 ? (
        <button className="bulk-delete" onClick={onDelete} type="button">
          删除选中（{count}）
        </button>
      ) : null}
    </div>
  );
}
