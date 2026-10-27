tableextension 50100 "FX Cust Ext A" extends Customer
{
    fields
    {
        field(50100; "FX A Code"; Code[10])
        {
            Caption = 'A Code';
        }
        field(50103; "FX A Mode"; Option)
        {
            OptionMembers = Off,On;
        }
    }

    var
        ALbl: Label 'A label';
}
