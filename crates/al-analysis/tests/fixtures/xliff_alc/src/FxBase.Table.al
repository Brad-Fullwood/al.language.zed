table 50100 "FX Base"
{
    Caption = 'FX Base';

    fields
    {
        field(1; "No."; Code[20])
        {
            Caption = 'No.';
        }
        field(2; Name; Text[50])
        {
        }
        field(3; Status; Option)
        {
            OptionMembers = Open,Closed;
            OptionCaption = 'Open,Closed';
        }
        field(4; Kind; Option)
        {
            OptionMembers = " ",A,B;
        }
        field(5; Described; Text[30])
        {
            Caption = 'Described', Comment = 'Explains the field';
        }
        field(6; Silent; Text[30])
        {
            Caption = 'Silent', Locked = true;
        }
    }
}
