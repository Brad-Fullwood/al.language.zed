// Test: Table with keys section (TARGET - currently failing)
table 50100 "Test Table Keys"
{
    fields
    {
        field(1; "No."; Code[20]) { }
        field(2; Name; Text[100]) { }
        field(3; Type; Integer) { }
    }
    
    keys
    {
        key(PK; "No.")
        {
            Clustered = true;
        }
        key(Key2; Name, Type)
        {
        }
    }
}
