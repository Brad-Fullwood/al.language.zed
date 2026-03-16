table 50100 "Test Customer"
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
        field(2; Name; Text[100])
        {
        }
        field(3; Balance; Decimal)
        {
        }
        field(4; "Contact Name"; Text[50])
        {
        }
        field(5; Active; Boolean)
        {
        }
    }

    keys
    {
        key(PK; "No.")
        {
            Clustered = true;
        }
        key(Name; Name)
        {
        }
    }
}
