/**
 * FindX 共享UI组件库
 * 统一的按钮、输入框、设置行等基础控件
 */

import type { ReactNode } from "react";
import "./ui-components.css";

/* ===== 按钮组件 ===== */

type ButtonProps = {
  children: ReactNode;
  onClick?: () => void;
  disabled?: boolean;
  variant?: "default" | "primary" | "ghost";
  size?: "sm" | "md" | "lg";
  type?: "button" | "submit" | "reset";
  className?: string;
  title?: string;
};

export function Button({
  children,
  onClick,
  disabled,
  variant = "default",
  size = "md",
  type = "button",
  className = "",
  title,
}: ButtonProps) {
  return (
    <button
      type={type}
      className={`fx-btn fx-btn--${variant} fx-btn--${size} ${className}`}
      onClick={onClick}
      disabled={disabled}
      title={title}
    >
      {children}
    </button>
  );
}

/* ===== 输入框组件 ===== */

type InputProps = {
  value: string | number;
  onChange: (value: string) => void;
  placeholder?: string;
  disabled?: boolean;
  type?: "text" | "number" | "password";
  min?: number;
  max?: number;
  className?: string;
};

export function Input({
  value,
  onChange,
  placeholder,
  disabled,
  type = "text",
  min,
  max,
  className = "",
}: InputProps) {
  return (
    <input
      type={type}
      className={`fx-input ${className}`}
      value={value}
      onChange={(e) => onChange(e.target.value)}
      placeholder={placeholder}
      disabled={disabled}
      min={min}
      max={max}
    />
  );
}

/* ===== 文本域组件 ===== */

type TextareaProps = {
  value: string;
  onChange: (value: string) => void;
  placeholder?: string;
  rows?: number;
  disabled?: boolean;
  className?: string;
};

export function Textarea({
  value,
  onChange,
  placeholder,
  rows = 4,
  disabled,
  className = "",
}: TextareaProps) {
  return (
    <textarea
      className={`fx-textarea ${className}`}
      value={value}
      onChange={(e) => onChange(e.target.value)}
      placeholder={placeholder}
      rows={rows}
      disabled={disabled}
    />
  );
}

/* ===== 复选框组件 ===== */

type CheckboxProps = {
  checked: boolean;
  onChange: (checked: boolean) => void;
  label: ReactNode;
  disabled?: boolean;
};

export function Checkbox({ checked, onChange, label, disabled }: CheckboxProps) {
  return (
    <label className="fx-checkbox">
      <input
        type="checkbox"
        checked={checked}
        onChange={(e) => onChange(e.target.checked)}
        disabled={disabled}
      />
      <span className="fx-checkbox-label">{label}</span>
    </label>
  );
}

/* ===== 单选框组组件 ===== */

type RadioOption<T extends string> = {
  value: T;
  label: ReactNode;
};

type RadioGroupProps<T extends string> = {
  name: string;
  value: T;
  options: RadioOption<T>[];
  onChange: (value: T) => void;
  disabled?: boolean;
  direction?: "row" | "column";
};

export function RadioGroup<T extends string>({
  name,
  value,
  options,
  onChange,
  disabled,
  direction = "column",
}: RadioGroupProps<T>) {
  return (
    <div className={`fx-radio-group fx-radio-group--${direction}`}>
      {options.map((opt) => (
        <label key={opt.value} className="fx-radio">
          <input
            type="radio"
            name={name}
            checked={value === opt.value}
            onChange={() => onChange(opt.value)}
            disabled={disabled}
          />
          <span className="fx-radio-label">{opt.label}</span>
        </label>
      ))}
    </div>
  );
}

/* ===== 设置行容器 ===== */

type SettingsRowProps = {
  children: ReactNode;
  label?: string;
  hint?: ReactNode;
  warning?: ReactNode;
  className?: string;
};

export function SettingsRow({ children, label, hint, warning, className = "" }: SettingsRowProps) {
  return (
    <div className={`fx-settings-row-container ${className}`}>
      {label && <label className="fx-settings-label">{label}</label>}
      {children}
      {hint && <p className="fx-settings-hint">{hint}</p>}
      {warning && <p className="fx-settings-warning">{warning}</p>}
    </div>
  );
}

/* ===== 提示消息 ===== */

type HintProps = {
  children: ReactNode;
  variant?: "info" | "warning" | "error";
  className?: string;
};

export function Hint({ children, variant = "info", className = "" }: HintProps) {
  return <p className={`fx-hint fx-hint--${variant} ${className}`}>{children}</p>;
}

/* ===== 分隔线 ===== */

export function Separator({ className = "" }: { className?: string }) {
  return <div className={`fx-separator ${className}`} />;
}
