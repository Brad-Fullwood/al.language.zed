tableextension 50101 "FX Cust Ext B" extends Customer
{
    fields
    {
        field(50101; "FX B Code"; Code[10])
        {
            Caption = 'B Code';
        }
        field(50102; "FX B Note"; Text[20])
        {
        }
    }

    var
        BLbl: Label 'B label';
}
