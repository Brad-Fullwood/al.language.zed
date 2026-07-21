table 50130 "Work Order Staging"
{
    DataClassification = CustomerContent;

    fields
    {
        field(1; "No."; Code[20])
        {
            trigger OnValidate()
            begin
                if "No." = '' then
                    Error('No. must not be empty');
            end;
        }
        field(2; Description; Text[100])
        {
        }
        field(3; Status; Enum "Work Order Status")
        {
        }
        field(4; "Journal Data"; Blob)
        {
        }
        field(5; Amount; Decimal)
        {
        }
    }

    keys
    {
        key(PK; "No.")
        {
            Clustered = true;
        }
    }

    procedure GetJournalData(): Text
    var
        InStream: InStream;
        JsonObj: JsonObject;
        Result: Text;
    begin
        CalcFields("Journal Data");
        "Journal Data".CreateInStream(InStream, TextEncoding::UTF8);
        InStream.ReadText(Result);
        JsonObj.ReadFrom(Result);
        exit(Result);
    end;
}
