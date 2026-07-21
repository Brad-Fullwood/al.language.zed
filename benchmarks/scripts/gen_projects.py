#!/usr/bin/env python3
"""Generate synthetic AL projects of varying sizes for benchmarking.

Each project gets N objects: N/2 tables and N/2 card pages. Objects are
deliberately non-trivial (fields, a procedure with StrSubstNo, an OnOpenPage
trigger calling a local procedure) so that parsing, binding and emit all have
real work to do.
"""
import json
import os
import shutil
import sys

SIZES = {"small": 2, "medium": 40, "large": 200, "xl": 800}

APP_JSON = {
    "id": "d0000000-0000-0000-0000-0000000000{:02d}",
    "name": "AL Bench {size}",
    "publisher": "Bench",
    "version": "1.0.0.0",
    "brief": "Synthetic benchmark project",
    "platform": "28.0.0.0",
    "application": "28.0.0.0",
    "runtime": "17.0",
    "idRanges": [{"from": 50000, "to": 69999}],
}

TABLE_TMPL = '''table {id} "Bench Table {n}"
{{
    DataClassification = CustomerContent;

    fields
    {{
        field(1; "Entry No."; Integer)
        {{
            DataClassification = CustomerContent;
            AutoIncrement = true;
        }}
        field(2; "Code"; Code[20])
        {{
            DataClassification = CustomerContent;
            NotBlank = true;
        }}
        field(3; Description; Text[100])
        {{
            DataClassification = CustomerContent;
        }}
        field(4; Amount; Decimal)
        {{
            DataClassification = CustomerContent;
            MinValue = 0;
        }}
        field(5; Active; Boolean)
        {{
            DataClassification = CustomerContent;
            InitValue = true;
        }}
    }}

    keys
    {{
        key(PK; "Entry No.")
        {{
            Clustered = true;
        }}
        key(ByCode; "Code", Amount)
        {{
        }}
    }}

    procedure Describe(): Text
    begin
        exit(StrSubstNo('%1 - %2 (%3)', "Code", Description, Amount));
    end;

    procedure Recalculate(Factor: Decimal): Decimal
    var
        Result: Decimal;
    begin
        Result := Amount * Factor;
        if Result < 0 then
            Result := 0;
        exit(Result);
    end;
}}
'''

PAGE_TMPL = '''page {id} "Bench Card {n}"
{{
    PageType = Card;
    ApplicationArea = All;
    UsageCategory = Administration;
    SourceTable = "Bench Table {tbl}";

    layout
    {{
        area(Content)
        {{
            group(General)
            {{
                field("Code"; Rec."Code")
                {{
                    ApplicationArea = All;
                    ToolTip = 'Specifies the code.';
                }}
                field(Description; Rec.Description)
                {{
                    ApplicationArea = All;
                    ToolTip = 'Specifies the description.';
                }}
                field(Amount; Rec.Amount)
                {{
                    ApplicationArea = All;
                    ToolTip = 'Specifies the amount.';
                }}
                field(Active; Rec.Active)
                {{
                    ApplicationArea = All;
                    ToolTip = 'Specifies whether the entry is active.';
                }}
            }}
        }}
    }}

    actions
    {{
        area(Processing)
        {{
            action(Refresh)
            {{
                ApplicationArea = All;
                Caption = 'Refresh';
                Image = Refresh;

                trigger OnAction()
                begin
                    CurrPage.Update(false);
                end;
            }}
        }}
    }}

    trigger OnOpenPage()
    begin
        InitializeFilter();
    end;

    local procedure InitializeFilter()
    begin
        Rec.SetRange(Active, true);
    end;
}}
'''


def gen(root: str, size: str, count: int) -> None:
    proj = os.path.join(root, size)
    if os.path.isdir(proj):
        shutil.rmtree(proj)
    src = os.path.join(proj, "src")
    os.makedirs(src)
    os.makedirs(os.path.join(proj, ".alpackages"))

    app = dict(APP_JSON)
    app["id"] = APP_JSON["id"].format(list(SIZES).index(size) + 1)
    app["name"] = APP_JSON["name"].format(size=size)
    with open(os.path.join(proj, "app.json"), "w") as fh:
        json.dump(app, fh, indent=2)

    n_tables = max(1, count // 2)
    n_pages = count - n_tables
    for i in range(n_tables):
        with open(os.path.join(src, f"Table_{50000 + i}.al"), "w") as fh:
            fh.write(TABLE_TMPL.format(id=50000 + i, n=i))
    for i in range(n_pages):
        with open(os.path.join(src, f"Page_{60000 + i}.al"), "w") as fh:
            fh.write(PAGE_TMPL.format(id=60000 + i, n=i, tbl=i % n_tables))

    print(f"{size}: {count} objects ({n_tables} tables, {n_pages} pages) -> {proj}")


if __name__ == "__main__":
    root = sys.argv[1] if len(sys.argv) > 1 else os.path.join(
        os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "projects")
    os.makedirs(root, exist_ok=True)
    for size, count in SIZES.items():
        gen(root, size, count)
