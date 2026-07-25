// Invalid: Table with keys but missing closing brace for fields section
table 50200 "Bad Table Keys"
{
    fields
    {
        field(1; "No."; Code[20]) { }
        field(2; Name; Text[100]) { }
    // Missing: }

    keys
    {
        key(PK; "No.")
        {
            Clustered = true;
        }
    }
}
