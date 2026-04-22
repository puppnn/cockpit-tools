import { useEffect, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { Check, ChevronDown } from "lucide-react";
import "./SingleSelectDropdown.css";

export interface SingleSelectOption {
  value: string;
  label: string;
  description?: string;
}

interface SingleSelectDropdownProps {
  value: string;
  options: SingleSelectOption[];
  onChange: (value: string) => void;
  disabled?: boolean;
  ariaLabel?: string;
  placeholder?: string;
  menuPlacement?: "down" | "up";
  menuMaxHeight?: number;
  menuMinWidth?: number;
  menuAlign?: "left" | "right";
}

export function SingleSelectDropdown({
  value,
  options,
  onChange,
  disabled = false,
  ariaLabel,
  placeholder,
  menuPlacement = "down",
  menuMaxHeight = 280,
  menuMinWidth,
  menuAlign = "left",
}: SingleSelectDropdownProps) {
  const [open, setOpen] = useState(false);
  const [menuStyle, setMenuStyle] = useState<{
    top?: number;
    bottom?: number;
    left: number;
    width: number;
    maxHeight: number;
  } | null>(null);
  const rootRef = useRef<HTMLDivElement | null>(null);
  const triggerRef = useRef<HTMLButtonElement | null>(null);
  const menuRef = useRef<HTMLDivElement | null>(null);

  const selectedOption = useMemo(
    () => options.find((option) => option.value === value) ?? null,
    [options, value],
  );

  useEffect(() => {
    if (!open) return;

    const updateMenuPosition = () => {
      const rect = triggerRef.current?.getBoundingClientRect();
      if (!rect) return;
      const autoMinWidth = options.some((option) => !!option.description) ? 220 : 0;
      const minimumWidth =
        typeof menuMinWidth === "number" ? menuMinWidth : autoMinWidth;
      const maxWidth = Math.max(160, window.innerWidth - 16);
      const width = Math.min(Math.max(rect.width, minimumWidth), maxWidth);
      const preferredLeft = menuAlign === "right" ? rect.right - width : rect.left;
      const maxLeft = Math.max(8, window.innerWidth - width - 8);
      const left = Math.min(Math.max(8, preferredLeft), maxLeft);
      if (menuPlacement === "up") {
        const availableHeight = Math.max(160, rect.top - 20);
        setMenuStyle({
          bottom: window.innerHeight - rect.top + 10,
          left,
          width,
          maxHeight: Math.min(menuMaxHeight, availableHeight),
        });
        return;
      }

      const availableHeight = Math.max(160, window.innerHeight - rect.bottom - 20);
      setMenuStyle({
        top: rect.bottom + 10,
        left,
        width,
        maxHeight: Math.min(menuMaxHeight, availableHeight),
      });
    };

    const handlePointerDown = (event: MouseEvent) => {
      const target = event.target as Node | null;
      if (!target) return;
      if (rootRef.current?.contains(target)) return;
      if (menuRef.current?.contains(target)) return;
      setOpen(false);
    };

    updateMenuPosition();
    document.addEventListener("mousedown", handlePointerDown);
    window.addEventListener("resize", updateMenuPosition);
    window.addEventListener("scroll", updateMenuPosition, true);

    return () => {
      document.removeEventListener("mousedown", handlePointerDown);
      window.removeEventListener("resize", updateMenuPosition);
      window.removeEventListener("scroll", updateMenuPosition, true);
    };
  }, [menuAlign, menuMaxHeight, menuMinWidth, menuPlacement, open, options]);

  useEffect(() => {
    if (!disabled) return;
    setOpen(false);
  }, [disabled]);

  const currentLabel = selectedOption?.label ?? placeholder ?? "";

  return (
    <div
      ref={rootRef}
      className={`single-select-dropdown${disabled ? " disabled" : ""}`}
    >
      <button
        ref={triggerRef}
        type="button"
        className={`single-select-dropdown-trigger${open ? " open" : ""}`}
        aria-label={ariaLabel}
        aria-haspopup="listbox"
        aria-expanded={open}
        onClick={() => {
          if (disabled) return;
          setOpen((prev) => !prev);
        }}
        disabled={disabled}
      >
        <span className="single-select-dropdown-value" title={currentLabel}>
          {currentLabel}
        </span>
        <span className="single-select-dropdown-arrow">
          <ChevronDown size={16} />
        </span>
      </button>

      {open && menuStyle
        ? createPortal(
            <div
              ref={menuRef}
              className="single-select-dropdown-menu"
              style={{
                position: "fixed",
                top: menuStyle.top !== undefined ? `${menuStyle.top}px` : "auto",
                bottom: menuStyle.bottom !== undefined ? `${menuStyle.bottom}px` : "auto",
                left: `${menuStyle.left}px`,
                width: `${menuStyle.width}px`,
                maxHeight: `${menuStyle.maxHeight}px`,
                zIndex: 11000,
              }}
              role="listbox"
              aria-label={ariaLabel}
            >
              {options.map((option) => {
                const active = option.value === value;
                const hasDescription = !!option.description;
                return (
                  <button
                    key={option.value}
                    type="button"
                    className={`single-select-dropdown-item${active ? " active" : ""}${
                      hasDescription ? " has-description" : ""
                    }`}
                    onClick={() => {
                      onChange(option.value);
                      setOpen(false);
                    }}
                    role="option"
                    aria-selected={active}
                  >
                    <span className="single-select-dropdown-item-content">
                      <span
                        className="single-select-dropdown-item-label"
                        title={option.label}
                      >
                        {option.label}
                      </span>
                      {hasDescription ? (
                        <span
                          className="single-select-dropdown-item-description"
                          title={option.description}
                        >
                          {option.description}
                        </span>
                      ) : null}
                    </span>
                    <span className="single-select-dropdown-item-check">
                      {active ? <Check size={15} /> : null}
                    </span>
                  </button>
                );
              })}
            </div>,
            document.body,
          )
        : null}
    </div>
  );
}
