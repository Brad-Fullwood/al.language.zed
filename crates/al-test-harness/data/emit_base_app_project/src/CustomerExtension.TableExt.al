tableextension 50120 "Emitter Customer Ext" extends Customer
{
    fields
    {
        field(50120; "Emitter Note"; Text[100])
        {
            Caption = 'Emitter Note';
            DataClassification = CustomerContent;
        }
    }
}
