import * as React from "react";

type ComboboxProps = {
    items: string[];
    value: string;
    onValueChange: (value: string) => void;
    disabled?: boolean;
    name?: string;
    inputRef?: React.Ref<HTMLInputElement>;
    onBlur?: React.FocusEventHandler<HTMLInputElement>;
    placeholder?: string;
    invalid?: boolean;
};

function Combobox({
    items,
    value,
    onValueChange,
    disabled = false,
    name,
    inputRef,
    onBlur,
    placeholder,
    invalid = false,
}: ComboboxProps) {
    const rootRef = React.useRef<HTMLDivElement>(null);
    const [open, setOpen] = React.useState(false);
    const [query, setQuery] = React.useState(value);
    const [highlightedIndex, setHighlightedIndex] = React.useState(0);
    const filteredItems = React.useMemo(() => {
        const normalized = query.trim().toLocaleLowerCase();
        return normalized
            ? items.filter((item) => item.toLocaleLowerCase().includes(normalized))
            : items;
    }, [items, query]);

    React.useEffect(() => {
        setHighlightedIndex(filteredItems.length ? 0 : -1);
    }, [filteredItems]);

    React.useEffect(() => {
        if (!open) return;
        const closeOnOutsidePointer = (event: PointerEvent) => {
            if (!rootRef.current?.contains(event.target as Node)) {
                setOpen(false);
                setQuery(value);
            }
        };
        document.addEventListener("pointerdown", closeOnOutsidePointer);
        return () => document.removeEventListener("pointerdown", closeOnOutsidePointer);
    }, [open, value]);

    function select(item: string) {
        onValueChange(item);
        setQuery(item);
        setOpen(false);
    }

    function handleKeyDown(event: React.KeyboardEvent<HTMLInputElement>) {
        if (event.key === "ArrowDown") {
            event.preventDefault();
            const wasOpen = open;
            setOpen(true);
            setHighlightedIndex((index) => filteredItems.length
                ? wasOpen ? (index + 1) % filteredItems.length : 0
                : -1);
        } else if (event.key === "ArrowUp") {
            event.preventDefault();
            const wasOpen = open;
            setOpen(true);
            setHighlightedIndex((index) => filteredItems.length
                ? wasOpen ? (index - 1 + filteredItems.length) % filteredItems.length : filteredItems.length - 1
                : -1);
        } else if (event.key === "Enter" && open && highlightedIndex >= 0) {
            event.preventDefault();
            const item = filteredItems[highlightedIndex];
            if (item) select(item);
        } else if (event.key === "Escape" && open) {
            event.preventDefault();
            setOpen(false);
            setQuery(value);
        }
    }

    return (
        <div className="category-combobox-anchor" ref={rootRef}>
            <input
                ref={inputRef}
                name={name}
                className="category-combobox-input"
                value={open ? query : value}
                placeholder={placeholder}
                disabled={disabled}
                autoComplete="off"
                role="combobox"
                aria-autocomplete="list"
                aria-expanded={open}
                aria-controls="business-category-options"
                aria-activedescendant={open && highlightedIndex >= 0
                    ? `business-category-option-${highlightedIndex}`
                    : undefined}
                aria-invalid={invalid}
                onFocus={() => {
                    setQuery(value);
                    setOpen(true);
                }}
                onChange={(event) => {
                    const nextQuery = event.currentTarget.value;
                    setQuery(nextQuery);
                    if (nextQuery !== value) onValueChange("");
                    setOpen(true);
                }}
                onBlur={(event) => {
                    onBlur?.(event);
                    setOpen(false);
                    setQuery(value);
                }}
                onKeyDown={handleKeyDown}
            />
            {open && (
                <ul
                    id="business-category-options"
                    className="category-combobox-options"
                    role="listbox"
                    onPointerDown={(event) => event.preventDefault()}
                >
                    {filteredItems.length ? filteredItems.map((item, index) => (
                        <li
                            id={`business-category-option-${index}`}
                            key={item}
                            className={index === highlightedIndex ? "highlighted" : ""}
                            role="option"
                            aria-selected={item === value}
                            onPointerEnter={() => setHighlightedIndex(index)}
                            onClick={() => select(item)}
                        >
                            {item}
                        </li>
                    )) : (
                        <li className="category-combobox-empty" role="status">No categories found.</li>
                    )}
                </ul>
            )}
        </div>
    );
}

export { Combobox };
