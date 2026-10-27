tableextension 50103 "FX Base Ext Z" extends "FX Base"
{
    fields
    {
        field(50103; "FX Z Code"; Code[10])
        {
            Caption = 'Z Code';
        }
    }

    var
        ZLbl: Label 'Z label';
}

tableextension 50102 "FX Base Ext Y" extends "FX Base"
{
    fields
    {
        field(50102; "FX Y Plain"; Code[10])
        {
        }
    }
}
