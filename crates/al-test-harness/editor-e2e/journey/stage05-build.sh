# Stage 5: add AL objects (ids 50140-50145, the CDX tenant has another app on 50100), build with the native emitter and with Microsoft alc, compare.
# shellcheck shell=bash

# journey_write_sources: table 50140, page extension on Customer Card, codeunit using the table.
journey_write_sources() {
  mkdir -p "$PROJ/src"
  cat > "$PROJ/src/JourneyItem.Table.al" <<'AL'
table 50140 "AZJ Journey Item"
{
    Caption = 'Journey Item';
    DataClassification = CustomerContent;

    fields
    {
        field(1; "Code"; Code[20])
        {
            Caption = 'Code';
            DataClassification = CustomerContent;
        }
        field(2; Description; Text[100])
        {
            Caption = 'Description';
            DataClassification = CustomerContent;
        }
        field(3; Quantity; Decimal)
        {
            Caption = 'Quantity';
            DataClassification = CustomerContent;
        }
        field(4; "Customer No."; Code[20])
        {
            Caption = 'Customer No.';
            DataClassification = CustomerContent;
            TableRelation = Customer;
        }
    }

    keys
    {
        key(PK; "Code")
        {
            Clustered = true;
        }
        key(Customer; "Customer No.")
        {
        }
    }
}
AL
  cat > "$PROJ/src/JourneyMgt.Codeunit.al" <<'AL'
codeunit 50141 "AZJ Journey Mgt"
{
    procedure AddItem(ItemCode: Code[20]; NewDescription: Text[100]; NewQuantity: Decimal; CustomerNo: Code[20])
    var
        JourneyItem: Record "AZJ Journey Item";
    begin
        JourneyItem.Init();
        JourneyItem.Code := ItemCode;
        JourneyItem.Description := NewDescription;
        JourneyItem.Quantity := NewQuantity;
        JourneyItem."Customer No." := CustomerNo;
        JourneyItem.Insert(true);
    end;

    procedure CountForCustomer(CustomerNo: Code[20]): Integer
    var
        JourneyItem: Record "AZJ Journey Item";
    begin
        JourneyItem.SetRange("Customer No.", CustomerNo);
        exit(JourneyItem.Count());
    end;

    procedure Double(Value: Integer): Integer
    begin
        exit(Value * 2);
    end;
}
AL
  cat > "$PROJ/src/JourneyCustomerCard.PageExt.al" <<'AL'
pageextension 50142 "AZJ Customer Card" extends "Customer Card"
{
    actions
    {
        addlast(processing)
        {
            action(AZJShowItemCount)
            {
                ApplicationArea = All;
                Caption = 'Journey Items';
                ToolTip = 'Shows how many journey items this customer has.';
                Image = Item;

                trigger OnAction()
                var
                    JourneyMgt: Codeunit "AZJ Journey Mgt";
                begin
                    Message('Journey items: %1', JourneyMgt.CountForCustomer(Rec."No."));
                end;
            }
        }
    }
}
AL
}

# journey_alc_build OUTAPP ITEM [STAGE]: runs Microsoft alc on $PROJ, logs to $OUT/STAGE/ITEM.log, error log to
# $OUT/STAGE/ITEM.errorlog.json. STAGE defaults to 05.
journey_alc_build() {
  local app="$1" item="$2" st="${3:-05}"
  run_step "$st" "$item" 600 dotnet "$ALC_DLL" "/project:$PROJ" "/packagecachepath:$PROJ/.alpackages" "/out:$app" "/errorlog:$OUT/$st/$item.errorlog.json"
}

# journey_native_codes LOG: "severity code" pairs from pack-native --json output (first log line is the command).
journey_native_codes() { tail -n +2 "$1" | jq -r '.diagnostics[]? | "\(.severity) \(.code)"' 2>/dev/null | sort; }
# journey_alc_codes ERRORLOG: "severity code" pairs from alc's /errorlog file.
journey_alc_codes() { jq -r '.issues[]? | "\(.properties.severity | ascii_downcase) \(.ruleId)"' "$1" 2>/dev/null | sort; }
# journey_errors FILE: count of error rows in a codes file.
journey_errors() { grep -c '^error ' "$1" || true; }

stage_05() {
  local s=05
  mkdir -p "$OUT/$s"
  if [[ ! -f "$PROJ/app.json" ]]; then record "$s" project SKIP "no project from stage 1"; return 0; fi
  journey_write_sources
  record "$s" sources PASS "table 50140, codeunit 50141, pageextension 50142 written"

  if [[ ! -s "$PROJ/.alpackages/System.app" ]]; then record "$s" symbols NIGGLE "no symbols in .alpackages (stage 2 not run or failed), builds will fail"; fi

  # --- good build, native then alc ---
  local nat="$OUT/$s/native.app" ref="$OUT/$s/alc.app"
  local nrc=0 arc=0
  run_step "$s" native 300 "$ALX" pack-native --project "$PROJ" --out "$nat" --json || nrc=$?
  if [[ "$nrc" == 0 && -s "$nat" ]]; then record "$s" native-build PASS "pack-native exit 0, $(stat -c %s "$nat") bytes"; else record "$s" native-build FAIL "pack-native exit $nrc, app present: $([[ -s $nat ]] && echo yes || echo no), see $s/native.log"; fi
  journey_alc_build "$ref" alc || arc=$?
  if [[ "$arc" == 0 && -s "$ref" ]]; then record "$s" alc-build PASS "alc exit 0, $(stat -c %s "$ref") bytes"; else record "$s" alc-build FAIL "alc exit $arc, app present: $([[ -s $ref ]] && echo yes || echo no), see $s/alc.log"; fi

  if [[ "$nrc" == 0 && "$arc" == 0 ]]; then record "$s" agree-good PASS "both succeed"; else record "$s" agree-good FAIL "native exit $nrc, alc exit $arc"; fi
  if [[ ! -s "$nat" && -s "$ref" ]]; then record "$s" app-present FAIL "alc wrote an app, native did not"; elif [[ -s "$nat" && ! -s "$ref" ]]; then record "$s" app-present FAIL "native wrote an app, alc did not"; elif [[ -s "$nat" ]]; then record "$s" app-present PASS "both wrote an .app"; fi

  journey_native_codes "$(step_log $s native)" > "$OUT/$s/native.codes"
  journey_alc_codes "$OUT/$s/alc.errorlog.json" > "$OUT/$s/alc.codes"
  if diff -q "$OUT/$s/native.codes" "$OUT/$s/alc.codes" > /dev/null; then record "$s" diag-codes-good PASS "same diagnostics ($(wc -l < "$OUT/$s/alc.codes") rows)"; else record "$s" diag-codes-good FAIL "native [$(tr '\n' ',' < "$OUT/$s/native.codes")] alc [$(tr '\n' ',' < "$OUT/$s/alc.codes")]"; fi

  # --- app content: the object entries both packages carry ---
  if [[ -s "$nat" && -s "$ref" ]]; then
    local nl al
    nl=$(unzip -l "$nat" 2>/dev/null | awk 'NR>3{print $4}' | sort | tr '\n' ' ')
    al=$(unzip -l "$ref" 2>/dev/null | awk 'NR>3{print $4}' | sort | tr '\n' ' ')
    unzip -l "$nat" > "$OUT/$s/native.zip.txt" 2>&1; unzip -l "$ref" > "$OUT/$s/alc.zip.txt" 2>&1
    if [[ "$nl" == "$al" ]]; then record "$s" package-entries PASS "same entry names"; else record "$s" package-entries NIGGLE "entry names differ (< native only, > alc only): $(diff <(tr " " "\n" <<<"$nl") <(tr " " "\n" <<<"$al") | grep "^[<>]" | tr "\n" " ")"; fi
  fi

  # --- deliberate errors, one semantic and one syntax, each built by both compilers ---
  local good="$OUT/$s/JourneyMgt.good.al" case_name
  cp "$PROJ/src/JourneyMgt.Codeunit.al" "$good"
  for case_name in semantic syntax; do
    cp "$good" "$PROJ/src/JourneyMgt.Codeunit.al"
    if [[ "$case_name" == semantic ]]; then
      sed -i 's/JourneyItem.Quantity := NewQuantity;/JourneyItem.NoSuchField := NewQuantity;/' "$PROJ/src/JourneyMgt.Codeunit.al"
    else
      sed -i 's/JourneyItem.Init();/JourneyItem.Init()/' "$PROJ/src/JourneyMgt.Codeunit.al"
    fi
    local nr=0 ar=0
    rm -f "$OUT/$s/native-$case_name.app" "$OUT/$s/alc-$case_name.app"
    run_step "$s" "native-$case_name" 300 "$ALX" pack-native --project "$PROJ" --out "$OUT/$s/native-$case_name.app" --json || nr=$?
    journey_alc_build "$OUT/$s/alc-$case_name.app" "alc-$case_name" || ar=$?
    local nsucc; nsucc=$(tail -n +2 "$(step_log $s "native-$case_name")" | jq -r '.success' 2>/dev/null)
    if [[ "$nr" != 0 || "$nsucc" == false ]]; then record "$s" "native-rejects-$case_name" PASS "pack-native exit $nr, success=$nsucc"; else record "$s" "native-rejects-$case_name" FAIL "pack-native exit $nr, success=$nsucc: the $case_name error was accepted and an .app written"; fi
    if [[ "$ar" != 0 ]]; then record "$s" "alc-rejects-$case_name" PASS "alc exit $ar"; else record "$s" "alc-rejects-$case_name" FAIL "alc accepted the $case_name error"; fi
    if [[ "$nr" == 0 && "$nsucc" != false && -s "$OUT/$s/native-$case_name.app" ]]; then record "$s" "native-bad-app-$case_name" FAIL "native wrote an .app for broken source"; fi
    if [[ -s "$OUT/$s/alc-$case_name.app" ]]; then record "$s" "alc-bad-app-$case_name" NIGGLE "alc wrote an .app despite the error"; fi
    journey_native_codes "$(step_log $s "native-$case_name")" > "$OUT/$s/native-$case_name.codes"
    journey_alc_codes "$OUT/$s/alc-$case_name.errorlog.json" > "$OUT/$s/alc-$case_name.codes"
    local ncount acount
    ncount=$(journey_errors "$OUT/$s/native-$case_name.codes"); acount=$(journey_errors "$OUT/$s/alc-$case_name.codes")
    if [[ "$ncount" == "$acount" ]]; then record "$s" "error-count-$case_name" PASS "both report $acount error(s)"; else record "$s" "error-count-$case_name" FAIL "native $ncount error(s), alc $acount"; fi
    if diff -q "$OUT/$s/native-$case_name.codes" "$OUT/$s/alc-$case_name.codes" > /dev/null; then record "$s" "diag-codes-$case_name" PASS "same codes: $(tr '\n' ',' < "$OUT/$s/alc-$case_name.codes")"; else record "$s" "diag-codes-$case_name" FAIL "native [$(tr '\n' ',' < "$OUT/$s/native-$case_name.codes")] alc [$(tr '\n' ',' < "$OUT/$s/alc-$case_name.codes")]"; fi
  done

  # restore the good source
  cp "$good" "$PROJ/src/JourneyMgt.Codeunit.al"
  local route; route=$(cat "$OUT/02/route.txt" 2>/dev/null || echo unknown)
  echo "alc compiled against symbols from route: $route" > "$OUT/$s/alc-symbols-route.txt"
  record "$s" alc-symbols PASS "alc compiled against symbols from the $route route"
  sync
}
